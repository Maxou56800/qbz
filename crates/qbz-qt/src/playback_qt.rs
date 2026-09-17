//! Minimal playback controller — drives `QbzCore`'s EXISTING public API
//! only (qbz-audio / qbz-player are protected; no audio logic is
//! reimplemented here). Ports the *shape* of `crates/qbz/src/playback.rs`:
//! play_album (fetch -> QueueTrack vec -> set_queue -> play_track_resolved)
//! and the poll loop (PlaybackEvent -> UI state, track-change meta refresh,
//! end-of-track advance).
//!
//! Frontend-owned playback responsibilities implemented here include:
//! - Stop-after-this-song (#35) — the end-of-track arm consumes the marker and
//!   pauses AHEAD of repeat/shuffle, and the gapless pre-queue guard that was
//!   already here now defends a marker the UI can actually place.
//! - Infinite refill (#33) — `try_infinite_refill` chains a smart artist radio
//!   off the ended track when the queue runs out and the ∞ toggle is on.
//! - Gapless prefetch: the engine raises `gapless_ready` ~10s out and this
//!   module now answers it with `play_next` (both the streaming and the
//!   local/DSD arms), so transitions are sample-accurate again.
//! - Blacklist filtering of the QUEUE (#1) + the unavailability drop (contract
//!   2026-08-17 §5.3 D5) — `filter_unplayable_queue` behind
//!   `set_queue_stamped` / `stamped`, the two seams every play/enqueue path
//!   already went through. Both seams DROP; `set_queue_stamped` additionally
//!   returns the surviving anchor, because a caller that computes its own
//!   `first_id` off the pre-filter list plays a track the core does not hold
//!   (F1 — silent, and reachable today through the blacklist alone).
//! - The bounded skip-unavailable walk (#2) — `auto_skip_unavailable`.
//! - Volume persistence (#3) — persisted on drag-end, restored at shell entry.
//! - The stream-failure toast with the Flatpak/Snap ALSA rewrite (#7) —
//!   `stream_error_text`, drained in the poll loop.
//! - The streaming seek lock (#15) — `seekable_max` from `buffer_progress`,
//!   published as `np_seekable_max`.
//! - Shuffle-play reorders the queue instead of latching the global shuffle
//!   MODE (#17) — `xorshift_shuffle` in `play_track_list_in`.
//! - Recently-played / Most-played recording — `recently_qt::record_queue_track`
//!   on the de-duped track edge, EPHEMERAL-EXEMPT: an ephemeral folder plays
//!   and leaves no row behind (owner rule, 2026-07-30). The Slint reference
//!   does NOT have that exemption; do not "restore parity" by removing it.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use cxx_qt_lib::QString;
use qbz_app::shell::AppRuntime;
use qbz_core::LoggingAdapter;
use qbz_models::{PageArtistTrack, Quality, QueueTrack, RepeatMode};
use qbz_player::PlaybackEvent;

/// A lease for work that belongs to the signed-in owner's local queue.
///
/// QConnect is initialized after the playback surface during shell boot, so a
/// missing service means the ordinary pre-QConnect local path is still valid.
/// Once the service exists, however, failure to obtain its permit means a
/// delegated renderer owns the queue (or an authority handoff is draining):
/// callers must treat the local action as handled/refused and do no work.
pub(crate) struct OwnerActionLease {
    _permit: Option<qconnect_app::AuthorityActionPermit>,
    token: OwnerActionToken,
}

/// Copyable stamp for a continuation derived from one exact owner snapshot.
///
/// `BeforeService` is intentionally invalidated as soon as the QConnect
/// singleton appears. Otherwise work queued during shell boot could be
/// re-admitted after an authority generation that did not exist when its
/// inputs were read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OwnerActionToken {
    BeforeService,
    Qconnect(qconnect_app::OwnerAuthorityToken),
}

/// Atomic classification used by playback observations that also own edge
/// bookkeeping. A fence is neither owner nor guest: it is a short retry window
/// while a still-fallible candidate drains owner work.
enum OwnerActionObservation {
    Owner(OwnerActionLease),
    Delegated,
    Fenced,
}

impl OwnerActionLease {
    pub(crate) const fn token(&self) -> OwnerActionToken {
        self.token
    }
}

/// A local transport control may operate on either owner or delegated audio,
/// but it still has to drain before an authority handoff can swap runtimes.
pub(crate) struct TransportActionLease {
    _permit: Option<qconnect_app::AuthorityActionPermit>,
}

pub(crate) fn begin_owner_action() -> Option<OwnerActionLease> {
    match crate::qconnect_qt::service() {
        None => Some(OwnerActionLease {
            _permit: None,
            token: OwnerActionToken::BeforeService,
        }),
        Some(service) => service
            .try_owner_action_permit_observed()
            .map(|(token, permit)| OwnerActionLease {
                _permit: Some(permit),
                token: OwnerActionToken::Qconnect(token),
            }),
    }
}

fn observe_owner_action() -> OwnerActionObservation {
    match crate::qconnect_qt::service() {
        None => OwnerActionObservation::Owner(OwnerActionLease {
            _permit: None,
            token: OwnerActionToken::BeforeService,
        }),
        Some(service) => match service.observe_owner_authority() {
            qconnect_app::OwnerAuthorityObservation::Owner { token, permit } => {
                OwnerActionObservation::Owner(OwnerActionLease {
                    _permit: Some(permit),
                    token: OwnerActionToken::Qconnect(token),
                })
            }
            qconnect_app::OwnerAuthorityObservation::Delegated => OwnerActionObservation::Delegated,
            qconnect_app::OwnerAuthorityObservation::Fenced => OwnerActionObservation::Fenced,
        },
    }
}

enum OwnerScopedSnapshot<T> {
    Captured {
        owner_action: Option<OwnerActionLease>,
        snapshot: T,
    },
    Fenced,
}

/// Freeze the authority observation before reading a local playback snapshot.
/// A fallible transition fence does not consume the snapshot at all; delegated
/// playback may still be read for UI/reporting but carries no owner permit.
fn capture_owner_scoped_snapshot<T>(
    observe: impl FnOnce() -> OwnerActionObservation,
    read: impl FnOnce() -> T,
) -> OwnerScopedSnapshot<T> {
    match observe() {
        OwnerActionObservation::Owner(owner_action) => OwnerScopedSnapshot::Captured {
            owner_action: Some(owner_action),
            snapshot: read(),
        },
        OwnerActionObservation::Delegated => OwnerScopedSnapshot::Captured {
            owner_action: None,
            snapshot: read(),
        },
        OwnerActionObservation::Fenced => OwnerScopedSnapshot::Fenced,
    }
}

fn owner_generation_changed(
    observed: Option<OwnerActionToken>,
    previous: Option<OwnerActionToken>,
) -> bool {
    observed.is_some() && observed != previous
}

/// Re-admit a queued continuation only for the authority generation that read
/// its input. This is deliberately different from [`begin_owner_action`]: a
/// guest -> owner promotion must not legitimize work derived from guest state.
pub(crate) async fn begin_owner_action_exact(token: OwnerActionToken) -> Option<OwnerActionLease> {
    match token {
        OwnerActionToken::BeforeService => {
            crate::qconnect_qt::service()
                .is_none()
                .then_some(OwnerActionLease {
                    _permit: None,
                    token,
                })
        }
        OwnerActionToken::Qconnect(token) => crate::qconnect_qt::service()?
            .wait_for_exact_owner_action_permit(token)
            .await
            .map(|permit| OwnerActionLease {
                _permit: Some(permit),
                token: OwnerActionToken::Qconnect(token),
            }),
    }
}

pub(crate) fn begin_transport_action() -> Option<TransportActionLease> {
    match crate::qconnect_qt::service() {
        None => Some(TransportActionLease { _permit: None }),
        Some(service) => service
            .try_transport_action_permit()
            .map(|permit| TransportActionLease {
                _permit: Some(permit),
            }),
    }
}

/// Cheap refusal check for route funnels. The real local operation always
/// acquires its own lease as well; this check only makes a delegated click look
/// handled instead of falling through to an error/toast path.
fn delegated_authority_owns_queue() -> bool {
    begin_owner_action().is_none()
}

/// The request tier for every play. Persisted in ui_prefs.json ("streaming_quality")
/// and applied here from Settings > Audio (settings_qt). Default "hires_plus".
static STREAMING_QUALITY: RwLock<Quality> = RwLock::new(Quality::UltraHiRes);

/// Map a ui_prefs quality key to the request tier (settings.rs STREAMING_QUALITIES).
fn quality_for_key(key: &str) -> Quality {
    match key {
        "mp3" => Quality::Mp3,
        "cd" => Quality::Lossless,
        "hires" => Quality::HiRes,
        _ => Quality::UltraHiRes,
    }
}

/// Settings > Audio > Streaming quality writes through here (also used at
/// startup to seed from the persisted prefs).
pub fn set_streaming_quality(key: &str) {
    *STREAMING_QUALITY.write().unwrap() = quality_for_key(key);
}

/// The raw streaming-quality PREFERENCE, uncapped.
///
/// Three callers, and that is the whole list by design: the capped resolver
/// below, and `cast_qt::effective_cast_quality`, which must NEVER see the
/// local cap (#638: the local DAC is not in a cast's signal path). Every
/// LOCAL play goes through `local_playback_quality` instead — reach for this
/// one only if you can say why a cap must not apply.
pub(crate) fn current_quality() -> Quality {
    *STREAMING_QUALITY.read().unwrap()
}

/// The request tier for LOCAL playback + the cause that shaped it — the one
/// place the preference and the detected device cap (#638 fix 3) are
/// reconciled. Port of `crates/qbz/src/playback.rs local_playback_quality`.
///
/// Costs two uncontended `RwLock` reads (`STREAMING_QUALITY` + the device-cap
/// cache), so it is safe to resolve per play; nothing here reads a file or
/// probes hardware. That is what lets the play funnel own the decision
/// instead of taking it as a parameter.
pub(crate) fn local_playback_quality() -> (Quality, qbz_models::QualityLimit) {
    reconcile_device_cap(
        current_quality(),
        qbz_app::device_cap::cap().map(|(t, _)| t),
    )
}

/// The decision itself, lifted out of the two lock reads so it is testable.
/// Port of the match in `crates/qbz/src/playback.rs local_playback_quality`.
fn reconcile_device_cap(
    pref: Quality,
    cap: Option<Quality>,
) -> (Quality, qbz_models::QualityLimit) {
    use qbz_models::QualityLimit;
    match cap {
        Some(cap) if cap < pref => (cap, QualityLimit::LocalDeviceCap),
        // The TIE arm, and it is not redundant: when the cap equals the
        // preference and both are below Hi-Res+, the device is the more
        // specific and more surprising of the two causes, so it is the one the
        // badge tooltip names. Without this the user reads "your preference"
        // while the real reason they cannot go higher is the DAC.
        Some(cap) if cap == pref && cap < Quality::UltraHiRes => {
            (pref, QualityLimit::LocalDeviceCap)
        }
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

/// How many immediate successors a new-track edge warms into L1/L2. This is
/// deliberately independent of the engine's `GAPLESS_LEAD_SECS`: the full
/// downloads begin as soon as the current track surfaces, while the existing
/// ~10 s gapless stage later consumes the first successor from cache (or its
/// normal fallback) and hands it to `play_next`.
const PREFETCH_LOOKAHEAD: usize = 2;
/// Seconds with nothing audible (local, remote or cast) before the poll loop
/// moves the L1 audio cache to disk. Five minutes: long enough that a pause
/// to answer the door keeps its warm cache, short enough that an app parked
/// after a session does not hold a listening session's worth of bytes.
const IDLE_L1_TRIM_SECS: u64 = 300;

static MEMORY_APPLY_BUSY: AtomicBool = AtomicBool::new(false);

pub(crate) fn memory_apply_busy() -> bool {
    MEMORY_APPLY_BUSY.load(Ordering::SeqCst)
}

/// Explicit, confirmed rebuild of local playback buffers. The authority lane
/// drains owner work and prevents the poller from repopulating gapless/L1 while
/// Stop is acknowledged. Disk cache, queue order and audio settings survive.
pub(crate) async fn apply_playback_memory(request: &str) -> Result<(), String> {
    let (mode, expected_track) = request.split_once(':')
        .filter(|(mode, _)| matches!(*mode, "idle" | "restart"))
        .ok_or("Playback changed. Apply again to confirm the current track.")?;
    if MEMORY_APPLY_BUSY.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() {
        return Ok(());
    }
    struct BusyGuard;
    impl Drop for BusyGuard {
        fn drop(&mut self) { MEMORY_APPLY_BUSY.store(false, Ordering::SeqCst); }
    }
    let _busy = BusyGuard;
    crate::settings_qt::publish_snapshot().await;
    let runtime = crate::APP.get().ok_or("Playback is not ready yet.")?;
    let service = crate::qconnect_qt::service().ok_or("Playback is not ready yet.")?;
    let (_lane, _fence) = tokio::time::timeout(
        std::time::Duration::from_secs(20), service.playback_memory_fence(),
    ).await.map_err(|_| "Playback is busy. Try applying the memory profile again.".to_string())??;
    if crate::cast_qt::is_casting().await || service.is_peer_renderer_active().await {
        return Err("Playback memory can only be applied to local playback.".into());
    }
    let player = runtime.core().player();
    let event = player.get_playback_event();
    let pending = PENDING_PLAY_ID.load(Ordering::Relaxed);
    let restart = event.is_playing || pending != 0;
    let active = player.has_loaded_audio() || event.is_playing || pending != 0;
    let current = runtime.core().current_track().await;
    if (restart && mode != "restart")
        || (active && !current.as_ref().is_some_and(|t| t.id.to_string() == expected_track)) {
        return Err("Playback changed. Apply again to confirm the current track.".into());
    }
    let _ = begin_resolved_play();
    PENDING_PLAY_ID.store(0, Ordering::Relaxed);
    player.stop_and_evict_playback_memory().await?;
    crate::now_playing::clear_loading();
    crate::now_playing::set_position(0, event.duration as i32, false, 0.0, false);
    if let Some(track) = current {
        let _ = qbz_app::session_persist::take_resume_for(track.id);
        if restart {
            crate::now_playing::begin_loading();
            let result = restore_owner_playback(runtime, track.id, 0, true).await;
            crate::now_playing::clear_loading();
            result?;
        }
    }
    refresh_now_playing(runtime).await;
    log::info!("[playback-memory] Applied profile; old playback buffers and L1 released, L2 retained");
    Ok(())
}

/// Shared upper bound across overlapping track edges. A new current track can
/// surface while the second successor of the previous edge is still warming;
/// the player's own fetching marker de-duplicates the overlap, while this
/// semaphore keeps unrelated downloads bounded.
const MAX_CONCURRENT_PREFETCH: usize = 2;
static PREFETCH_SEMAPHORE: tokio::sync::Semaphore =
    tokio::sync::Semaphore::const_new(MAX_CONCURRENT_PREFETCH);

/// Every background download that can retain an owner-action permit. The
/// delegation host closes admission first, then advances the generation and
/// aborts this registry before waiting for the authority drain. A start gate
/// makes registration race-free: a task cannot touch I/O until its AbortHandle
/// is either visible to cancellation or rejected by the newer generation.
static OWNER_PLAYBACK_TASK_GENERATION: AtomicU64 = AtomicU64::new(0);
static OWNER_PLAYBACK_TASKS: OnceLock<Mutex<Vec<(u64, tokio::task::AbortHandle)>>> =
    OnceLock::new();

/// Frontend generation for the policy work that happens before the player's
/// own `begin_play`: preference lookup and bounded filesystem probes. The
/// player protects its fetch/decode stages, but it cannot fence work that has
/// not called it yet. A newer resolved start invalidates an older lookup
/// before that lookup is allowed to materialize a ticket or enter fallback.
static RESOLVED_PLAY_GENERATION: AtomicU64 = AtomicU64::new(0);

fn begin_resolved_play() -> u64 {
    RESOLVED_PLAY_GENERATION
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |generation| {
            generation.checked_add(1)
        })
        .expect("resolved playback generation exhausted")
        + 1
}

fn resolved_play_is_current(generation: u64) -> bool {
    RESOLVED_PLAY_GENERATION.load(Ordering::SeqCst) == generation
}

fn owner_playback_tasks() -> &'static Mutex<Vec<(u64, tokio::task::AbortHandle)>> {
    OWNER_PLAYBACK_TASKS.get_or_init(|| Mutex::new(Vec::new()))
}

fn owner_playback_task_generation() -> u64 {
    OWNER_PLAYBACK_TASK_GENERATION.load(Ordering::SeqCst)
}

fn spawn_owner_playback_task<F>(future: F) -> tokio::task::JoinHandle<()>
where
    F: std::future::Future<Output = ()> + Send + 'static,
{
    let generation = owner_playback_task_generation();
    let (start_tx, start_rx) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        if start_rx.await.is_err() || owner_playback_task_generation() != generation {
            return;
        }
        future.await;
    });
    let abort = task.abort_handle();
    let should_start = {
        let mut tasks = owner_playback_tasks()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        tasks.retain(|(_, handle)| !handle.is_finished());
        if owner_playback_task_generation() == generation {
            tasks.push((generation, abort.clone()));
            true
        } else {
            false
        }
    };
    if should_start {
        let _ = start_tx.send(());
    } else {
        abort.abort();
    }
    task
}

/// Cancel downloads that were admitted before a transition fence. Callers
/// must close owner admission first; advancing the generation then makes a
/// concurrent registration either visible in the registry or self-rejecting.
pub(crate) fn cancel_owner_playback_tasks() {
    OWNER_PLAYBACK_TASK_GENERATION
        .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |generation| {
            generation.checked_add(1)
        })
        .expect("owner playback task generation exhausted");
    let handles = {
        let mut tasks = owner_playback_tasks()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        tasks
            .drain(..)
            .map(|(_, handle)| handle)
            .collect::<Vec<_>>()
    };
    for handle in handles {
        handle.abort();
    }
}

/// The prefetch edge is separate from `last_track_id`: the latter belongs to
/// the LOCAL UI/integration pump and is intentionally reset while casting,
/// whereas immediate warming must follow whichever transport owns playback.
/// Zero means no owner/current track and rearms a later replay of the same id.
#[derive(Debug, Default)]
struct PrefetchTrackEdge {
    last_track_id: u64,
}

impl PrefetchTrackEdge {
    fn observe(&mut self, track_id: u64) -> bool {
        if track_id == 0 {
            self.last_track_id = 0;
            return false;
        }
        if track_id == self.last_track_id {
            return false;
        }
        self.last_track_id = track_id;
        true
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PrefetchCandidate {
    id: u64,
    is_local: bool,
}

impl From<&QueueTrack> for PrefetchCandidate {
    fn from(track: &QueueTrack) -> Self {
        Self {
            id: track.id,
            is_local: track.is_local,
        }
    }
}

/// Pure policy seam for the deterministic contract tests. `upcoming` is
/// already the queue's repeat/shuffle-aware next-two snapshot; filtering a
/// local row does not reach beyond that snapshot and accidentally warm a third
/// successor.
fn prefetch_track_ids(
    streaming_only: bool,
    offline: bool,
    throttle_cap: usize,
    upcoming: &[PrefetchCandidate],
) -> Vec<u64> {
    if streaming_only || offline || throttle_cap == 0 {
        return Vec::new();
    }
    upcoming
        .iter()
        .take(PREFETCH_LOOKAHEAD)
        .filter(|track| track.id != 0 && !track.is_local)
        .map(|track| track.id)
        .collect()
}

fn prefetch_quality_tag(quality: Quality) -> qbz_audio::network_throttle::PlaybackQualityTag {
    use qbz_audio::network_throttle::PlaybackQualityTag;
    match quality {
        Quality::UltraHiRes => PlaybackQualityTag::UltraHiRes,
        Quality::HiRes => PlaybackQualityTag::HiRes,
        Quality::Lossless => PlaybackQualityTag::Lossless,
        Quality::Mp3 => PlaybackQualityTag::Lossy,
    }
}

/// Streaming-only gapless policy. Cached/offline successors stay on the
/// established byte path; only a cold network successor needs the bounded
/// initial-buffer handoff. A zero adaptive cap protects a live stream that is
/// already struggling instead of competing with it for the link.
#[derive(Debug, PartialEq, Eq)]
enum GaplessSource {
    Download,
    Catalog,
    Local,
}

fn gapless_source(track: &QueueTrack) -> GaplessSource {
    if crate::local_playback::belongs_to_the_offline_tier(track) {
        GaplessSource::Download
    } else if track.is_local {
        GaplessSource::Local
    } else {
        GaplessSource::Catalog
    }
}

fn should_stream_gapless_successor(
    _streaming_only: bool,
    player_cached: bool,
    offline_cached: bool,
    throttle_cap: usize,
) -> bool {
    !offline_cached && (player_cached || throttle_cap > 0)
}

/// The queue/engine edge a completed gapless fetch still belongs to.
///
/// Full-track materialization can outlive the predecessor. Checking only the
/// requested successor lets a late result append that successor behind itself
/// after the normal end-edge already started it. Queue edits and a late
/// stop-after mark are equally stale. Keep the policy pure so all of those
/// races have deterministic coverage.
fn gapless_edge_matches(
    predecessor_id: u64,
    successor_id: u64,
    engine_track_id: u64,
    engine_queued_id: u64,
    stop_after_id: Option<u64>,
    queue_successor_id: Option<u64>,
) -> bool {
    predecessor_id != 0
        && successor_id != 0
        && predecessor_id != successor_id
        && engine_track_id == predecessor_id
        && engine_queued_id == 0
        && stop_after_id != Some(predecessor_id)
        && queue_successor_id == Some(successor_id)
}

async fn gapless_edge_is_current(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    predecessor_id: u64,
    successor_id: u64,
) -> bool {
    let player = runtime.core().player();
    let engine_track_id = player.state.current_track_id();
    let engine_queued_id = player.state.get_gapless_next_track_id();
    let stop_after_id = runtime.core().get_stop_after().await;
    let queue_successor_id = runtime
        .core()
        .peek_upcoming(1)
        .await
        .first()
        .map(|track| track.id);
    gapless_edge_matches(
        predecessor_id,
        successor_id,
        engine_track_id,
        engine_queued_id,
        stop_after_id,
        queue_successor_id,
    )
}

/// Warm the next two queue rows in the player's shared L1/L2 cache. This only
/// performs cheap policy/queue work inline; every full download is spawned in
/// the background and goes through `Player::prefetch_into_cache`, which owns
/// the in-flight marker, cache re-check, 20 s failure cooldown and cache write.
async fn kick_prefetch(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    owner_snapshot: &OwnerActionLease,
) {
    let owner_token = owner_snapshot.token();
    let streaming_only = crate::settings_qt::audio_settings().streaming_only;
    let offline = crate::offline_fwd::engine().is_offline();
    if streaming_only || offline {
        return;
    }

    // Resolve once so the bandwidth estimate and every spawned request agree.
    // A renderer owns its own cap; the local DAC cap must never leak into the
    // quality-blind cache that the cast path reads first.
    let quality = crate::cast_qt::service()
        .casting_prefetch_quality()
        .await
        .unwrap_or_else(|| local_playback_quality().0);
    let memory = runtime.core().player().playback_prefetch_policy();
    if qbz_app::memory_watchdog::prefetch_halted()
        || ((!memory.allow_hires || qbz_app::memory_watchdog::hires_prefetch_paused())
            && matches!(quality, Quality::HiRes | Quality::UltraHiRes)) { return; }
    let throttle_cap = qbz_audio::network_throttle::state().current_prefetch_cap(
        qbz_audio::network_throttle::playback_mbps_for_quality(prefetch_quality_tag(quality)),
        MAX_CONCURRENT_PREFETCH.min(memory.concurrency),
    );
    if throttle_cap == 0 {
        log::debug!("[qbz-qt] prefetch: skipped (network throttle cap 0)");
        return;
    }

    let upcoming = runtime.core().peek_upcoming(PREFETCH_LOOKAHEAD.min(memory.lookahead)).await;
    let candidates: Vec<PrefetchCandidate> = upcoming.iter().map(PrefetchCandidate::from).collect();
    let track_ids = prefetch_track_ids(streaming_only, offline, throttle_cap, &candidates);
    for track_id in track_ids {
        let player = runtime.core().player();
        if player.is_track_cached(track_id) {
            continue;
        }
        let runtime = runtime.clone();
        drop(spawn_owner_playback_task(async move {
            let _permit = match PREFETCH_SEMAPHORE.acquire().await {
                Ok(permit) => permit,
                Err(_) => return,
            };
            let Some(_owner_action) = begin_owner_action_exact(owner_token).await else {
                return;
            };
            let client_lock = runtime.core().client();
            let guard = client_lock.read().await;
            let Some(client) = guard.as_ref() else {
                return;
            };
            let player = runtime.core().player();
            if !player.playback_prefetch_policy().allow_hires && matches!(quality, Quality::HiRes | Quality::UltraHiRes) { return; }
            if let Err(error) = player.prefetch_into_cache(client, track_id, quality).await {
                log::debug!("[qbz-qt] prefetch: track {track_id} failed: {error}");
            }
        }));
    }
}

// Mute bookkeeping (mirrors playback.rs MUTED / PREMUTE_VOLUME: there is no
// dedicated mute API on the core — mute is volume 0 with a stash).
/// The track id of a COLD START that has been dispatched but has not yet
/// surfaced on the poll loop's track-change edge. 0 = nothing in flight.
///
/// It exists because the cold-start branch in `toggle_play` opened a window the
/// resume branch never had: between dispatch and the first audio frame the
/// engine reports `is_playing == false` AND `has_loaded_audio() == false` — the
/// exact condition that branch tests — so a second Play press would dispatch a
/// SECOND `play_track_resolved` for the same id, racing the first. On this port
/// that window is seconds, not milliseconds (the ACTIVE_TASK notes an
/// unexplained 5459 ms CMAF init).
///
/// THE SHAPE MATTERS, and it is the reference's, not Tauri's. Tauri had a
/// consumed-once `pendingSessionRestore` token that it cleared BEFORE awaiting
/// the load and never re-armed in the catch (`playerStore.ts:522`, `:603-608`),
/// so ONE failed load consumed the restore intent and every later Play press
/// fell through to a bare resume against an empty engine — the Play button was
/// dead for the rest of the session, silently. That is a large part of why this
/// feature "nunca quedaba bien" there.
///
/// This latch cannot do that, because it never gates the ABILITY to start:
///  - Its branch CANCELS the in-flight load (stop + clear) rather than
///    refusing, so a press always does something and always leaves a state the
///    next press can act on.
///  - It is cleared on the track edge AND on every failure path, so a failed
///    cold start re-opens the cold-start branch instead of latching it shut.
///  - Even if it were somehow stranded set, the next press stops and clears it,
///    and the press after that cold-starts normally. It self-heals in both
///    directions; a consumed-once token heals in neither.
///
/// This is also the reference's #661 fix (`crates/qbz/src/playback.rs:4811-4824`):
/// a pause issued during the load window used to fall through to the resume
/// branch and be swallowed — the track started anyway and the sleep inhibitor
/// stayed held with no successful pause to release it.
static PENDING_PLAY_ID: AtomicU64 = AtomicU64::new(0);

static MUTED: AtomicBool = AtomicBool::new(false);
static PREMUTE_VOLUME: AtomicU64 = AtomicU64::new(0);

// ---------------------------------------------------------------------------
// Playback CONTEXT — the "playing from" origin (song-card layers glyph)
// ---------------------------------------------------------------------------

/// The container a queue was launched FROM. `kind` is "album" | "artist" |
/// "playlist" | "label" and `id` that container's navigation id — 1:1 with the
/// Slint `open-context(kind, id)` -> `media-action(kind, id, "open")` arms
/// (qbz/src/main.rs:12695 artist, :12701 album, :12707 playlist, :13206 label).
///
/// HARDENING (owner ask): the origin is NOT something a play path has to
/// remember. It travels WITH the queue — every play/enqueue entry point in this
/// module funnels through `set_queue_stamped` / `stamped`, which stamp it onto
/// EVERY track, and when the caller passes none it is DERIVED from the queue
/// itself (`derive_context`). A new entry point therefore cannot silently ship
/// an unstamped queue; the worst case is the same album fallback the Slint uses
/// in `refresh_now_playing_meta` (playback.rs:1959-1965).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayContext {
    pub kind: String,
    pub id: String,
}

impl PlayContext {
    /// None for an empty kind/id — an empty context must never be stamped (it
    /// would shadow the per-track album fallback with a dead glyph).
    pub fn new(kind: &str, id: &str) -> Option<Self> {
        if kind.is_empty() || id.is_empty() {
            return None;
        }
        Some(Self {
            kind: kind.to_string(),
            id: id.to_string(),
        })
    }

    pub fn album(id: &str) -> Option<Self> {
        Self::new("album", id)
    }

    pub fn artist(id: &str) -> Option<Self> {
        Self::new("artist", id)
    }

    pub fn playlist(id: &str) -> Option<Self> {
        Self::new("playlist", id)
    }

    /// A label page's Popular Tracks. The reference stamps this on BOTH label
    /// play paths — the header Shuffle (`playback.rs:3149`) and a row click
    /// (`:3541`) — and it cannot be derived: a label's popular tracks share
    /// neither an album nor an artist, so `derive_context` returns None and
    /// the queue would ship contextless (PARITY-DEBT #17's reference path).
    pub fn label(id: &str) -> Option<Self> {
        Self::new("label", id)
    }
}

/// Infer the container from the queue itself: one shared album -> that album,
/// otherwise one shared artist -> that artist (an artist page's Popular Tracks
/// span many albums but ONE artist — the exact case that shipped contextless).
/// Nothing shared -> None, and the per-track album fallback takes over.
fn derive_context(tracks: &[QueueTrack]) -> Option<PlayContext> {
    let mut album: Option<&str> = None;
    let mut one_album = true;
    let mut artist: Option<u64> = None;
    let mut one_artist = true;

    for track in tracks {
        match track.album_id.as_deref().filter(|s| !s.is_empty()) {
            None => one_album = false,
            Some(id) => {
                if let Some(prev) = album {
                    if prev != id {
                        one_album = false;
                    }
                } else {
                    album = Some(id);
                }
            }
        }
        match track.artist_id {
            None => one_artist = false,
            Some(id) => {
                if let Some(prev) = artist {
                    if prev != id {
                        one_artist = false;
                    }
                } else {
                    artist = Some(id);
                }
            }
        }
    }

    // ORDER IS DELIBERATE — album before artist. Do not flip it: a one-album
    // queue (the common case: an album play whose producer forgot to stamp)
    // legitimately wants the ALBUM origin, and an artist page's Popular Tracks
    // only reach the artist arm because they span albums. Producers that know
    // better (playlist / artist page / label) pass an EXPLICIT context and
    // never depend on this order — this is the safety net for the next
    // producer someone adds, not the primary mechanism.
    if one_album {
        if let Some(id) = album {
            return PlayContext::album(id);
        }
    }
    // A SINGLE track that shares "one artist" is not an artist container — it
    // is a bare track play, whose Slint fallback is its own album. Only a
    // multi-track queue earns the artist origin.
    if one_artist && tracks.len() > 1 {
        if let Some(id) = artist {
            return PlayContext::artist(&id.to_string());
        }
    }
    None
}

/// Stamp the origin onto every track that does not already carry one. Tracks
/// that arrive pre-stamped (the album/playlist/local builders) keep theirs, so
/// a mixed queue stays honest per row.
fn stamp_context(tracks: &mut [QueueTrack], explicit: Option<PlayContext>) {
    let resolved = match explicit {
        Some(ctx) => Some(ctx),
        None => derive_context(tracks),
    };
    let Some(ctx) = resolved else {
        return;
    };
    for track in tracks.iter_mut() {
        let already_stamped = track.context_kind.as_deref().is_some_and(|k| !k.is_empty())
            && track.context_id.as_deref().is_some_and(|i| !i.is_empty());
        if already_stamped {
            continue;
        }
        track.context_kind = Some(ctx.kind.clone());
        track.context_id = Some(ctx.id.clone());
    }
}

/// THE blacklist drop predicate for a built queue row (playback.rs:2624-2632
/// `queue_track_blacklisted`). Local / Plex / no-id rows fail open — the
/// underlying `is_track_blacklisted` returns false for any source that is not
/// "qobuz", so a local file can never be dropped by an artist block.
fn queue_track_blacklisted(track: &QueueTrack) -> bool {
    let source = track.source.as_deref().unwrap_or("qobuz");
    crate::artist_blacklist::is_track_blacklisted(
        source,
        track.artist_id,
        None,
        track.album_id.as_deref(),
    )
}

/// THE unavailable drop predicate for a built queue row (contract §5.1 with the
/// §5.3 / F5 exemption). Two halves, and the second is what keeps this feature
/// from becoming a worse bug than the one it fixes:
///
/// 1. `!track.streamable` — the API said `streamable: false` for this track.
///    `QueueTrack.streamable` defaults **true** (`qbz-models/src/playback.rs`
///    `default_streamable`), so a builder that never learned the answer fails
///    OPEN and the track plays. That default is deliberate, not laziness:
///    refusing to play because an endpoint was terse is strictly worse than
///    playing something that turns out to be dead, and the reactive
///    `auto_skip_unavailable` walk still catches the second case.
/// 2. `&& !is_cached_id(..)` — minus the tracks we already downloaded.
///    `qbz-core`'s play resolution takes the offline tier BEFORE any network
///    call and before any availability question, and every Qt play hands the
///    handle down (`play_resolved_offline_aware`), so a track Qobuz pulled from
///    the catalogue plays perfectly from disk TODAY. Dropping it would take
///    away a song the user deliberately downloaded, and it would bite hardest
///    in offline mode, where that copy is the only one that exists. The Slint
///    reference gates on exactly this (`crates/qbz/src/playback.rs:426`).
///
/// LOCAL / Plex rows can never be dropped here, and that is not luck: their
/// builder (`local_playback::local_queue_track`) sets `streamable: true`
/// because a file on disk is outside Qobuz's streaming rights entirely.
pub(crate) fn queue_track_unavailable(track: &QueueTrack) -> bool {
    // Clause 2 (D3): a track the player already found terminally unavailable
    // THIS RUN counts as dead even while the API keeps advertising it as
    // streamable — which is the owner's test case exactly, since `/album/get`
    // is honest but plenty of rows only fail at the stream-url step. The cache
    // exemption applies to both clauses: downloaded bytes play regardless.
    let dead =
        !track.streamable || crate::track_replace_qt::session_unavailable::contains(track.id);
    dead && !crate::offline_qt::is_cached_id(track.id)
}

/// Why rows were dropped, counted per reason. The reasons are NOT interchangeable
/// and must not collapse into one number: a blacklist drop is what the user
/// asked for and stays SILENT, while an unavailability drop is news they never
/// asked for and owes them a toast (D4 — "el usuario siempre debe saber"). One
/// counter would force one policy on both.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct QueueDrops {
    /// Dropped by the artist blacklist. Silent by design.
    pub blacklisted: usize,
    /// Dropped because Qobuz pulled the track AND we hold no offline copy.
    pub unavailable: usize,
    /// Dropped while QConnect is enabled because its source is not resolvable
    /// by Qobuz. These rows never enter the local queue in that mode.
    pub qconnect_unresolvable: usize,
}

/// The ONE toast a queue build owes when it silently shortened itself
/// (§5.3). Once per build, never once per track — an album with six pulled
/// tracks must not raise six toasts.
///
/// PLURAL msgid pair, per acceptance §8-6: a singular-only string reads wrong
/// in every locale with plural rules, Russian worst of all. The pair follows
/// the catalog's existing convention (`"{} duplicate skipped"` /
/// `"{} duplicates skipped"`). The i18n lane lands it in all EIGHT locales;
/// nothing else in this module is a new msgid.
fn toast_unavailable_dropped(count: usize) {
    if count == 0 {
        return;
    }
    crate::toast_qt::info(qbz_i18n::tf(
        "{} track skipped — no longer available",
        "{} tracks skipped — no longer available",
        count as i64,
        &[&count.to_string()],
    ));
}

/// Drop the rows that must never reach the queue AND remap the start index onto
/// the surviving track (playback.rs:2634-2641 `filter_blacklisted_queue`, plus
/// the unavailability drop of contract §5.3 D5).
///
/// The Slint applies the blacklist at ~18 separate builder sites. This port
/// funnels every play path through `set_queue_stamped`, so it needs exactly one
/// — which is also why its absence was total rather than partial: the store was
/// open and the manager worked, but nothing between the two ever asked.
///
/// The start remap is the part the Slint gets for free by filtering before it
/// computes the index. Here the caller has already picked an index into the
/// UNFILTERED list, so dropping rows ahead of it would silently start the wrong
/// track. The clicked row surviving is the common case; when the clicked row is
/// itself dropped, the walk lands on the next survivor, which is what the user
/// asking for "play from here" means once "here" is gone.
///
/// Renamed from `filter_blacklisted_queue`: it now drops for three reasons and
/// the old name named only one of them.
fn filter_unplayable_queue(
    tracks: Vec<QueueTrack>,
    start: Option<usize>,
) -> (Vec<QueueTrack>, Option<usize>, QueueDrops) {
    let qconnect_enabled = crate::qconnect_qt::queue_admission_enabled();
    let (kept, start, dropped) = filter_queue_with(
        tracks,
        start,
        queue_track_blacklisted,
        queue_track_unavailable,
        |track| qconnect_enabled && !crate::qconnect_qt::is_qconnect_queue_track(track),
    );
    if dropped.blacklisted > 0 {
        log::info!(
            "[qbz-qt] blacklist: dropped {} track(s) from the queue",
            dropped.blacklisted
        );
    }
    if dropped.unavailable > 0 {
        log::info!(
            "[qbz-qt] unavailable: dropped {} pulled track(s) from the queue",
            dropped.unavailable
        );
    }
    if dropped.qconnect_unresolvable > 0 {
        log::info!(
            "[qbz-qt] qconnect admission: dropped {} unresolvable track(s) from the queue",
            dropped.qconnect_unresolvable
        );
    }
    (kept, start, dropped)
}

/// The pure half, split out so the index remap is unit-testable without a bound
/// session (the blacklist store needs one). The three predicates decide;
/// everything else is arithmetic, and that arithmetic is unchanged.
///
/// Separate predicates, not one OR'd closure, because the caller has to know WHICH
/// one claimed each row (see [`QueueDrops`]). `blacklisted` is tested FIRST and
/// wins ties: a pulled track by an artist the user blocked is a row they
/// already chose not to see, and toasting "1 track skipped" for it would
/// announce exactly the content the blacklist exists to hide.
///
/// The log line that used to live here moved to `filter_unplayable_queue`: it
/// said "blacklist", which any additional reason turns into a lie.
fn filter_queue_with<T>(
    tracks: Vec<T>,
    start: Option<usize>,
    blacklisted: impl Fn(&T) -> bool,
    unavailable: impl Fn(&T) -> bool,
    qconnect_unresolvable: impl Fn(&T) -> bool,
) -> (Vec<T>, Option<usize>, QueueDrops) {
    let clicked = start.unwrap_or(0);
    let mut kept = Vec::with_capacity(tracks.len());
    // Surviving index for the clicked row, or for the first survivor after it.
    let mut new_start: Option<usize> = None;
    let mut dropped = QueueDrops::default();
    for (i, track) in tracks.into_iter().enumerate() {
        if blacklisted(&track) {
            dropped.blacklisted += 1;
            continue;
        }
        if unavailable(&track) {
            dropped.unavailable += 1;
            continue;
        }
        if qconnect_unresolvable(&track) {
            dropped.qconnect_unresolvable += 1;
            continue;
        }
        if new_start.is_none() && i >= clicked {
            new_start = Some(kept.len());
        }
        kept.push(track);
    }
    // Every survivor sits BEFORE the clicked row (the whole tail was dropped):
    // start at the last one rather than past the end.
    let start = new_start
        .or_else(|| kept.len().checked_sub(1))
        .filter(|_| !kept.is_empty());
    (kept, start, dropped)
}

/// What the queue seam actually published: the start index AFTER the filter,
/// and the id of the track sitting at it.
///
/// F1, and it is a blocker (contract §5.3). Every caller used to read `first_id`
/// from the UNFILTERED list, hand the list to this seam, and then play the id
/// the seam had just dropped. `play_track_resolved` returns `Err`, the caller
/// `log::error!`s it, and the USER GETS SILENCE with no toast. That is not a
/// hypothetical introduced by the availability work — it is reachable today
/// through the artist blacklist: click a blocked artist's album and nothing
/// plays. Returning the post-filter anchor is what makes "the id the caller
/// plays" and "the queue the core holds" one list again.
///
/// `None` means NOTHING survived. The caller must not play and must not report
/// an error: the seam has already told the user why (the skipped toast) and has
/// already left the previous queue untouched rather than publishing an empty
/// one over it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct QueueStart {
    /// Index into the queue the core now holds.
    pub index: usize,
    /// The track at `index` — the id to hand to `play_resolved_offline_aware`
    /// and to `route_play_to_peer`.
    pub track_id: u64,
}

/// A replacement queue after blacklist/availability filtering and context
/// stamping, but before it is visible to the core or any UI surface.
///
/// Source-owned playback can fail before the player accepts new audio (dead
/// Plex part, unavailable NAS, server outage). Preparing first lets those
/// callers prove the anchor is audible without making Queue, NPB and MPRIS
/// announce it over the track that is still actually playing.
pub(crate) struct PreparedQueue {
    tracks: Vec<QueueTrack>,
    start: Option<usize>,
    anchor: Option<QueueStart>,
    unavailable_dropped: usize,
    qconnect_unresolvable_dropped: usize,
}

impl PreparedQueue {
    pub(crate) fn anchor_track(&self) -> Option<&QueueTrack> {
        self.start.and_then(|index| self.tracks.get(index))
    }
}

pub(crate) fn prepare_queue_stamped(
    tracks: Vec<QueueTrack>,
    start: Option<usize>,
    context: Option<PlayContext>,
) -> Option<PreparedQueue> {
    // Source-owned playback stages audio before committing the replacement,
    // so it reaches this lower seam directly. Refuse before that staging work
    // when a delegated renderer owns the queue.
    let _owner_action = begin_owner_action()?;
    let asked = tracks.len();
    let (mut tracks, start, dropped) = filter_unplayable_queue(tracks, start);
    if tracks.is_empty() && asked > 0 {
        log::info!(
            "[qbz-qt] set_queue: all {asked} track(s) filtered ({} blacklisted, {} unavailable, {} qconnect-unresolvable) \
             — keeping the previous queue",
            dropped.blacklisted,
            dropped.unavailable,
            dropped.qconnect_unresolvable
        );
        toast_unavailable_dropped(dropped.unavailable);
        crate::qconnect_qt::toast_unresolvable_tracks(dropped.qconnect_unresolvable);
        return None;
    }
    stamp_context(&mut tracks, context);
    warn_dead_context(&tracks, "set_queue");
    let anchor = start.and_then(|index| {
        tracks.get(index).map(|track| QueueStart {
            index,
            track_id: track.id,
        })
    });
    Some(PreparedQueue {
        tracks,
        start,
        anchor,
        unavailable_dropped: dropped.unavailable,
        qconnect_unresolvable_dropped: dropped.qconnect_unresolvable,
    })
}

pub(crate) async fn commit_prepared_queue(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    prepared: PreparedQueue,
) -> Option<QueueStart> {
    commit_prepared_queue_with_offline_only(runtime, prepared, false).await
}

async fn commit_prepared_queue_with_offline_only(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    prepared: PreparedQueue,
    offline_only: bool,
) -> Option<QueueStart> {
    let Some(_owner_action) = begin_owner_action() else {
        // Preserve the anchor as a handled/refused result. Callers route it
        // through `route_play_remote` next, which consumes the delegated case
        // without starting local audio or surfacing a misleading filter error.
        return prepared.anchor;
    };
    let PreparedQueue {
        tracks,
        start,
        anchor,
        unavailable_dropped,
        qconnect_unresolvable_dropped,
    } = prepared;
    runtime
        .core()
        .set_queue_with_offline_only(tracks, start, offline_only)
        .await;
    toast_unavailable_dropped(unavailable_dropped);
    crate::qconnect_qt::toast_unresolvable_tracks(qconnect_unresolvable_dropped);
    if let Some(svc) = crate::qconnect_qt::service() {
        svc.sync_local_queue_if_changed().await;
    }
    anchor
}

/// The ONLY `core().set_queue` call in this module — every play path goes
/// through it, so the origin can never be dropped on the floor, and neither can
/// the blacklist (PARITY-DEBT #1) nor a track Qobuz pulled (contract §5.3 D5).
///
/// Returns the surviving anchor; see [`QueueStart`] for why no caller may
/// compute it itself any more.
pub(crate) async fn set_queue_stamped(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    tracks: Vec<QueueTrack>,
    start: Option<usize>,
    context: Option<PlayContext>,
) -> Option<QueueStart> {
    let prepared = prepare_queue_stamped(tracks, start, context)?;
    commit_prepared_queue(runtime, prepared).await
}

/// Local-playlist variant of [`set_queue_stamped`] that publishes the D8
/// offline-only flag before the queue event becomes observable.
pub(crate) async fn set_queue_stamped_with_offline_only(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    tracks: Vec<QueueTrack>,
    start: Option<usize>,
    context: Option<PlayContext>,
    offline_only: bool,
) -> Option<QueueStart> {
    let prepared = prepare_queue_stamped(tracks, start, context)?;
    commit_prepared_queue_with_offline_only(runtime, prepared, offline_only).await
}

/// Same guarantee for the ADD paths (play-next / add-to-queue): appended tracks
/// carry their own origin, so the glyph stays right after the queue advances
/// into them.
///
/// `pub(crate)` on purpose: the local/ephemeral/bulk enqueue paths live in
/// other modules and must be able to reach the SAME stamping seam — a private
/// helper is what forced them to hand-roll their context in the first place.
pub(crate) fn stamped(tracks: Vec<QueueTrack>, context: Option<PlayContext>) -> Vec<QueueTrack> {
    if delegated_authority_owns_queue() {
        // Enqueue callers already treat an empty post-seam batch as a handled
        // no-op. Do not toast: the controller owns the visible guest queue.
        return Vec::new();
    }
    // The SECOND seam (F2). `set_queue_stamped` guards the REPLACE paths; this
    // guards play-next / play-later / add-to-queue, and its own reason for
    // existing is that enqueue "must not be the back door" for the blacklist.
    // D5 makes the same demand of a pulled track: a row the user cannot click
    // in the list must not be reachable by dragging it into the queue either.
    // No start index to remap here — these APPEND.
    let mut dropped = QueueDrops::default();
    let qconnect_enabled = crate::qconnect_qt::queue_admission_enabled();
    let mut tracks: Vec<QueueTrack> = tracks
        .into_iter()
        .filter(|t| {
            if queue_track_blacklisted(t) {
                dropped.blacklisted += 1;
                return false;
            }
            if queue_track_unavailable(t) {
                dropped.unavailable += 1;
                return false;
            }
            if qconnect_enabled && !crate::qconnect_qt::is_qconnect_queue_track(t) {
                dropped.qconnect_unresolvable += 1;
                return false;
            }
            true
        })
        .collect();
    if dropped.blacklisted > 0 {
        log::info!(
            "[qbz-qt] blacklist: dropped {} track(s) from an enqueue",
            dropped.blacklisted
        );
    }
    if dropped.unavailable > 0 {
        log::info!(
            "[qbz-qt] unavailable: dropped {} pulled track(s) from an enqueue",
            dropped.unavailable
        );
    }
    if dropped.qconnect_unresolvable > 0 {
        log::info!(
            "[qbz-qt] qconnect admission: dropped {} unresolvable track(s) from an enqueue",
            dropped.qconnect_unresolvable
        );
    }
    // Idempotent with the seam below it: `myqbz_play_qt` stamps and THEN calls
    // `set_queue_stamped`, whose second pass finds nothing left to drop and so
    // raises no second toast. That module's existing "both passes are
    // idempotent" comment stays true.
    toast_unavailable_dropped(dropped.unavailable);
    crate::qconnect_qt::toast_unresolvable_tracks(dropped.qconnect_unresolvable);
    stamp_context(&mut tracks, context);
    warn_dead_context(&tracks, "enqueue");
    tracks
}

// ---------------------------------------------------------------------------
// QConnect routing helpers (contract §6.3/§7 — Slint playback.rs:4082-4111
// plus the routed arms on every Qobuz-castable add path)
// ---------------------------------------------------------------------------

/// QConnect sync-on-add predicate (#442, playback.rs:4082-4096): true when
/// every surviving row uses a Qobuz-resolvable provenance. The shared enqueue
/// seam normally guarantees this while QConnect is enabled; this remains a
/// defensive gate for callers running while it is disabled or starting up.
pub(crate) fn batch_all_qconnect_castable(tracks: &[QueueTrack]) -> bool {
    tracks
        .iter()
        .all(crate::qconnect_qt::is_qconnect_queue_track)
}

/// QConnect sync-on-add (#442, playback.rs:4098-4111): after a local queue
/// ADD (play-next / play-later / add-to-queue), push the updated queue to the
/// session immediately so a connected controller sees the new item AT its
/// position — previously it only re-synced on the next track transition.
/// `sync_local_queue_if_changed` self-gates (no-op when not connected, when a
/// peer owns playback, or when the queue is unchanged), so this is cheap in
/// every non-renderer situation.
pub(crate) async fn sync_qconnect_after_add(added_castable: bool) {
    if !added_castable {
        return;
    }
    let Some(_owner_action) = begin_owner_action() else {
        return;
    };
    if let Some(svc) = crate::qconnect_qt::service() {
        svc.sync_local_queue_if_changed().await;
    }
}

/// QConnect CONTROLLER-mode enqueue routing (contract §7 — the Slint add-path
/// arms, e.g. playback.rs:3012-3023, 4116-4128): when a PEER renderer owns
/// playback, route the add to the peer's queue instead of mutating only the
/// LOCAL queue (the peer never sees a local-only enqueue; the cloud echoes a
/// QueueUpdated that `materialize_remote_queue` applies). Mode-matched to the
/// caller's LOCAL placement: "next" inserts after the peer's current track,
/// "later" lands on the peer's manual-block tail, anything else appends.
/// Returns `true` when handled remotely — the caller MUST NOT enqueue
/// locally, and its sync tail does not run (the peer owns the queue now).
pub(crate) async fn route_enqueue_to_peer(tracks: &[QueueTrack], mode: &str) -> bool {
    let Some(_owner_action) = begin_owner_action() else {
        return true;
    };
    let Some(svc) = crate::qconnect_qt::service() else {
        return false;
    };
    let routed: Vec<(u64, Option<String>)> =
        tracks.iter().map(|qt| (qt.id, qt.source.clone())).collect();
    match mode {
        "next" => svc.play_next_batch_on_peer_if_active(&routed).await,
        "later" => svc.play_later_batch_on_peer_if_active(&routed).await,
        _ => svc.add_to_queue_batch_on_peer_if_active(&routed).await,
    }
}

/// Single-track variant of [`route_enqueue_to_peer`] (Slint
/// playback.rs:4120-4128's single-track arm).
pub(crate) async fn route_track_to_peer(track: &QueueTrack, mode: &str) -> bool {
    let Some(_owner_action) = begin_owner_action() else {
        return true;
    };
    let Some(svc) = crate::qconnect_qt::service() else {
        return false;
    };
    match mode {
        "next" => {
            svc.play_next_on_peer_if_active(track.id, track.source.as_deref())
                .await
        }
        "later" => {
            svc.play_later_on_peer_if_active(track.id, track.source.as_deref())
                .await
        }
        _ => {
            svc.add_to_queue_on_peer_if_active(track.id, track.source.as_deref())
                .await
        }
    }
}

/// The one funnel every local play goes through. It OWNS four decisions so
/// no caller has to remember them:
///
/// 1. the request tier — `local_playback_quality()`, which is where the
///    detected device cap (#638 fix 3) applies. It used to be a PARAMETER,
///    which is why seven callers pasted the same `current_quality()` and four
///    others bypassed the funnel entirely;
/// 2. the ACTIVE offline cache handle, and
/// 3. the row-status sink (Slint passes both through `play_track_resolved`;
///    this port used to pass `None, None`, which made the offline tier
///    unreachable — downloaded tracks always streamed);
/// 4. session resume — and note this one is NOT gated on the offline cache:
///    a caller passing 0 gets the restore stash consumed if it is armed for
///    this exact track (once only). That matches the reference, whose audible
///    play calls `take_resume_for` unconditionally.
///
/// KNOWN LIMIT, and it is the honest reading of "the funnel owns the tier":
/// it owns the tier we REQUEST. An offline copy already on disk is served
/// verbatim — `qbz-core` ignores the quality argument on that branch, because
/// downloads are always taken at the top tier. So a capped device still gets
/// the full-rate bytes of a track the user downloaded. Same on the reference;
/// fixing it means transcoding or a second download, neither of which this
/// row is allowed to decide.
///
/// NOT its job: `route_play_to_peer`. Two call-site shapes forbid hoisting it
/// here — `play_queue_track` needs the peer route BEFORE its local-file
/// branch, and the MyQBZ collection play deliberately does not early-return on
/// a routed play (its `touch_play` bookkeeping runs either way).
pub(crate) async fn play_resolved_offline_aware(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    track_id: u64,
    start_position_secs: u64,
) -> Result<(), String> {
    let Some(_owner_action) = begin_owner_action() else {
        log::debug!(
            "[play] local resolved start for track {track_id} refused by delegated authority"
        );
        return Ok(());
    };
    let play_generation = begin_resolved_play();
    // TRIPWIRE for the funnel: this is the LOCAL Qobuz/offline step and must
    // never run while a renderer owns playback. A hit here means an entry
    // point skipped `route_play_remote` — it is a bug, and this line is how
    // the next one gets caught in a log instead of in a smoke.
    if crate::cast_qt::is_casting().await {
        log::warn!("[play] LOCAL PLAY WHILE CASTING (bug): track {track_id} reached play_resolved_offline_aware");
    }
    // SOURCE FIRST (design 02 §9 stage 3). Everything below this block is the
    // QOBUZ entry: the tier walk, the offline cache, the quality request. A row
    // a different source owns has no business in it, and until now it went
    // there anyway — `myqbz_play_qt`'s own header called this out as the reason
    // a MyQBZ collection's local or Plex first track "still fails AT THE
    // PLAYER even though it now resolves".
    //
    // It was also unsafe, not merely wrong: the offline cache is keyed on the
    // raw `u64`, and a `local_tracks` rowid or a Plex rating key lives in the
    // same small-integer space as older Qobuz ids. A collision played THE
    // WRONG TRACK rather than failing cleanly (#638 fix 3's sharp edge).
    // Claiming the row first removes the id-space overlap: a claimed non-Qobuz
    // row never reaches the cache lookup at all.
    //
    // The cursor test is deliberate. Every caller sets the queue and then plays
    // its anchor, so the core is already sitting on this track; if it is NOT,
    // this function does exactly what it did before rather than guessing which
    // row was meant.
    let logical_track = runtime
        .core()
        .current_track()
        .await
        .filter(|t| t.id == track_id);
    if let Some(qt) = logical_track.as_ref() {
        let raw = qbz_source::RawRef::from_queue_track(&qt);
        // OFFLINE rows stay on THIS path on purpose — see
        // `local_playback::play_current_if_local` for the log line that proves
        // why. The offline tier below is the only thing that can read a
        // CMAF-cached download, and the row carries the Qobuz catalog id so it
        // can.
        let is_offline = raw.badge == qbz_source::SourceBadge::Offline;
        if let (false, Ok(item)) = (is_offline, qbz_source::registry().claim(&raw)) {
            if item.source() != qbz_source::SourceId::QOBUZ {
                if !resolved_play_is_current(play_generation) {
                    return Ok(());
                }
                return match crate::audible_qt::play_queue_track(runtime, qt).await {
                    Ok(true) => Ok(()),
                    // Its own source owned it and could not play it. Phrased so
                    // `is_terminal_unavailable` routes it into the SAME bounded
                    // skip walk a pulled Qobuz track takes — to the user the two
                    // failures are the same one.
                    Ok(false) => Err(format!("local file no longer available (track {track_id})")),
                    Err(e) => Err(e.to_string()),
                };
            }
        }
    }

    let quality = local_playback_quality().0;
    let off = crate::offline_qt::get().await;
    let sink = off.as_ref().map(|_| crate::offline_cache_qt::row_sink());
    if !resolved_play_is_current(play_generation) {
        return Ok(());
    }
    // Session resume: if this is the track restored at launch, start it at the
    // saved position (consumed once). Only when the caller did not ask for a
    // position itself — an explicit seek-then-play must win over the restore.
    let start_position_secs = if start_position_secs == 0 {
        qbz_app::session_persist::take_resume_for(track_id)
    } else {
        start_position_secs
    };
    if !resolved_play_is_current(play_generation) {
        return Ok(());
    }
    // The queue row retains its Qobuz identity, so explicitly forget the
    // previous byte source before resolving this start. A purchase success
    // records its exact format below; Qobuz mode and every fallback leave the
    // row clear and therefore restore the catalog badge in the NPB.
    crate::purchase_playback_qt::clear_materialized_track(track_id);
    if let Some(track) = logical_track.as_ref() {
        match try_play_preferred_purchase(runtime, track, start_position_secs, play_generation)
            .await
        {
            PreferredPurchaseStart::Played | PreferredPurchaseStart::Superseded => return Ok(()),
            PreferredPurchaseStart::Fallback => {}
        }
    }
    if !resolved_play_is_current(play_generation) {
        return Ok(());
    }
    runtime
        .core()
        .play_track_resolved(
            track_id,
            quality,
            off.as_deref(),
            sink.as_ref(),
            start_position_secs,
        )
        .await
}

/// Materialize one Qobuz queue row from the user's exact album preference.
/// Every candidate was already proven complete and healthy by the blocking
/// resolver. A decoder/open failure advances to another complete physical
/// copy of the same format before returning control to the Qobuz tier walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreferredPurchaseStart {
    Played,
    Fallback,
    Superseded,
}

async fn try_play_preferred_purchase(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    track: &QueueTrack,
    start_position_secs: u64,
    play_generation: u64,
) -> PreferredPurchaseStart {
    let Some(resolution) =
        crate::purchase_playback_qt::resolve_preferred_copies(track.id, track.album_id.as_deref())
            .await
    else {
        return if resolved_play_is_current(play_generation) {
            PreferredPurchaseStart::Fallback
        } else {
            PreferredPurchaseStart::Superseded
        };
    };

    for copy in &resolution.copies {
        if !resolved_play_is_current(play_generation) {
            log::debug!(
                "[qbz-qt] purchase: discarded stale resolution for track {}",
                track.id
            );
            return PreferredPurchaseStart::Superseded;
        }
        let accepted = if copy.is_dsd() {
            crate::audible_qt::play_ticket(runtime, copy.ticket(track.id, 0)).await
        } else {
            // The generic File ticket performs its read and then immediately
            // calls `play_data`. Keep those phases apart here so a newer play
            // can supersede a slow NAS read before the old bytes touch the
            // player. Whole-file PCM is already the established File-ticket
            // policy; DSD remains streaming-by-path above.
            let path = copy.path.clone();
            let bytes = tokio::task::spawn_blocking(move || std::fs::read(path))
                .await
                .ok()
                .and_then(Result::ok);
            if !resolved_play_is_current(play_generation) {
                return PreferredPurchaseStart::Superseded;
            }
            match bytes {
                Some(bytes) => {
                    crate::audible_qt::play_ticket(
                        runtime,
                        qbz_source::PlaybackTicket::Bytes {
                            bytes,
                            play_id: track.id,
                        },
                    )
                    .await
                }
                None => false,
            }
        };
        if !accepted {
            log::warn!(
                "[qbz-qt] purchase: copy {} of track {} (format {}) was refused; trying the next exact copy",
                copy.copy_id,
                track.id,
                copy.format_id
            );
            continue;
        }

        // DSD tickets are file-only, but the current direct DSD engines accept
        // the ordinary player seek. Apply every cold-resume/takeback offset
        // after the play command, generation-guarded. This keeps DSD local and
        // avoids the generic File ticket's unguarded post-read delay for PCM.
        if start_position_secs > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(120)).await;
            if !resolved_play_is_current(play_generation) {
                return PreferredPurchaseStart::Superseded;
            }
            if let Err(error) = runtime.core().player().seek(start_position_secs) {
                log::warn!(
                    "[qbz-qt] purchase: local seek for track {} to {}s failed: {error}; using Qobuz",
                    track.id,
                    start_position_secs
                );
                let _ = runtime.core().player().stop();
                continue;
            }
        }

        if !resolved_play_is_current(play_generation) {
            return PreferredPurchaseStart::Superseded;
        }

        crate::purchase_playback_qt::remember_materialized_copy(track.id, &resolution, copy);
        log::info!(
            "[qbz-qt] purchase: playing track {} from copy {} (format {})",
            track.id,
            copy.copy_id,
            copy.format_id
        );
        return PreferredPurchaseStart::Played;
    }

    log::warn!(
        "[qbz-qt] purchase: all complete copies of track {} (format {}) failed; falling back to Qobuz",
        track.id,
        resolution.format_id
    );
    if resolved_play_is_current(play_generation) {
        PreferredPurchaseStart::Fallback
    } else {
        PreferredPurchaseStart::Superseded
    }
}

/// Prepare a purchase successor without allowing a slow file read or DSD→PCM
/// conversion to append after its queue edge has changed. Native/DoP DSD
/// retains the existing path ticket because that append is immediate; every
/// byte materialization returns here first and is revalidated by the caller.
async fn prepare_purchase_gapless_ticket(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    copy: &crate::purchase_playback_qt::PurchasedCopy,
    track_id: u64,
) -> Result<qbz_source::PlaybackTicket, String> {
    if copy.is_dsd() && runtime.core().player().is_dsd_direct_active() {
        return Ok(copy.ticket(track_id, 0));
    }

    let path = copy.path.clone();
    let is_dsd = copy.is_dsd();
    let bytes = tokio::task::spawn_blocking(move || {
        if is_dsd {
            qbz_player::Player::prepare_dsd_gapless_wav(&path)
        } else {
            std::fs::read(path).map_err(|error| error.to_string())
        }
    })
    .await
    .map_err(|error| format!("purchase gapless preparation task failed: {error}"))??;
    Ok(qbz_source::PlaybackTicket::Bytes {
        bytes,
        play_id: track_id,
    })
}

/// Restore the signed-in owner's exact current track after delegated playback.
///
/// The delegation host calls this while owning its transition gate and closed
/// owner-action fence (the offline path uses the same exclusive handoff
/// boundary). This helper therefore does not acquire authority again and
/// deliberately skips both Cast and QConnect-peer routing: it restores audio
/// on this process from the queue snapshot the host already installed.
/// `position_secs` is always explicit, including zero, so the process-start
/// session-resume stash can never override the handoff snapshot.
pub(crate) async fn restore_owner_playback(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    track_id: u64,
    position_secs: u64,
    was_playing: bool,
) -> Result<(), String> {
    let track = runtime
        .core()
        .current_track()
        .await
        .filter(|track| track.id == track_id)
        .ok_or_else(|| "owner restore cursor no longer matches its snapshot".to_string())?;

    // A startup resume token for the same track is stale once this stronger
    // authority snapshot exists. Consume and discard it even when the exact
    // handoff position is zero.
    let _ = qbz_app::session_persist::take_resume_for(track_id);
    let play_generation = begin_resolved_play();

    let raw = qbz_source::RawRef::from_queue_track(&track);
    let is_offline = raw.badge == qbz_source::SourceBadge::Offline;
    let source_item = if is_offline {
        None
    } else {
        match qbz_source::registry().claim(&raw) {
            Ok(item) => Some(item),
            Err(error) if track.is_local => {
                return Err(format!(
                    "owner source restore could not claim its row: {error}"
                ));
            }
            // Legacy Qobuz queue snapshots may predate the explicit `source`
            // field. Preserve their catalog fallback when `is_local` is false.
            Err(_) => None,
        }
    };

    if source_item
        .as_ref()
        .is_some_and(|item| item.source() != qbz_source::SourceId::QOBUZ)
    {
        match crate::audible_qt::play_queue_track(runtime, &track).await {
            Ok(true) => {
                // Source tickets start at their own zero (or CUE base). Apply a
                // non-zero relative owner position after the source is live;
                // zero remains an explicit fresh start and never consults the
                // process-start resume stash above.
                if position_secs > 0 {
                    runtime
                        .core()
                        .seek(position_secs)
                        .map_err(|error| format!("owner source restore seek failed: {error}"))?;
                }
            }
            Ok(false) => return Err("owner source restore was refused by the player".to_string()),
            Err(error) => return Err(format!("owner source restore failed: {error}")),
        }
    } else {
        match try_play_preferred_purchase(runtime, &track, position_secs, play_generation).await {
            PreferredPurchaseStart::Played => {}
            PreferredPurchaseStart::Superseded => return Ok(()),
            PreferredPurchaseStart::Fallback => {
                if !resolved_play_is_current(play_generation) {
                    return Ok(());
                }
                let quality = local_playback_quality().0;
                let offline = crate::offline_qt::get().await;
                let sink = offline
                    .as_ref()
                    .map(|_| crate::offline_cache_qt::row_sink());
                runtime
                    .core()
                    .play_track_resolved(
                        track_id,
                        quality,
                        offline.as_deref(),
                        sink.as_ref(),
                        position_secs,
                    )
                    .await?;
            }
        }
    }

    if !was_playing {
        runtime
            .core()
            .pause()
            .map_err(|error| format!("owner restore pause failed: {error}"))?;
    }
    Ok(())
}

/// QConnect CONTROLLER-mode play routing (contract §7's decided arm position —
/// Slint playback.rs:638-651): when a PEER renderer owns playback, route the
/// play to the peer instead of running the local audible step. Call AFTER
/// `set_queue_stamped` (the arm pushes the CURRENT core queue — placed before
/// the funnel it would push the STALE one) and BEFORE `play_track_resolved`.
/// On handled, refreshes the bar + queue from the core cursor (the Slint
/// `after_track_change` meta refresh + sidebar) and returns `true` — the
/// caller early-returns and MUST NOT play locally. The Slint's `clear_loading`
/// after `play_on_peer_if_active` (playback.rs:643-650) maps to
/// `now_playing::clear_loading()` here: the poll loop's peer branch normally
/// clears the spinner via its position push (`has_audio = true`) on the next
/// tick, but a REFUSED routed play (mixed queue) never primes the peer — a
/// paused+idle peer would leave the spinner latched (sign-off F1).
/// THE remote-renderer gate — the one place every audible start passes
/// before any local backend is touched: CAST first (a connected renderer
/// owns playback), then the QConnect PEER (controller mode), else `false`
/// = play locally. Same order as the queue funnel always had.
///
/// Why one seam: the cast gate lived ONLY in `play_queue_track`, while the
/// peer gate had been added to every other entry (`play_single_track`,
/// `play_album*`, `play_artist`, `play_track_list_in`, playlist, MyQBZ, the
/// Local Library funnel) as `route_play_to_peer` — so a track CLICK while
/// casting started the local backend beside the renderer (2026-08-29 smoke,
/// twice). Nothing may call `route_play_to_peer` directly any more; `entry`
/// names the caller in the log so the next missing gate is visible.
pub(crate) async fn route_play_remote(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    track_id: u64,
    entry: &'static str,
) -> bool {
    // This play decision supersedes any local purchase bytes previously
    // associated with the catalog id. A local purchase resolution records the
    // format again after the player accepts it; Cast/QConnect/local-source
    // routes leave it clear so the NPB never inherits a stale purchase badge.
    crate::purchase_playback_qt::clear_materialized_track(track_id);
    let Some(_owner_action) = begin_owner_action() else {
        log::debug!("[play] entry={entry} track={track_id} -> refused by delegated authority");
        crate::now_playing::clear_loading();
        return true;
    };
    if crate::cast_qt::play_current_if_cast(runtime).await {
        log::info!("[play] entry={entry} track={track_id} -> cast renderer");
        crate::now_playing::clear_loading();
        refresh_now_playing(runtime).await;
        publish_queue(runtime).await;
        return true;
    }
    if route_play_to_peer(runtime, track_id).await {
        log::info!("[play] entry={entry} track={track_id} -> qconnect peer");
        return true;
    }
    log::debug!("[play] entry={entry} track={track_id} -> local");
    false
}

/// `route_play_remote` for a funnel that holds the TRACK before the queue
/// cursor moves (the Local Library stages its queue after the audible step):
/// the renderer gets this track, not whatever the old queue pointed at.
pub(crate) async fn route_play_remote_track(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    track: &QueueTrack,
    entry: &'static str,
) -> bool {
    crate::purchase_playback_qt::clear_materialized_track(track.id);
    let Some(_owner_action) = begin_owner_action() else {
        log::debug!(
            "[play] entry={entry} track={} -> refused by delegated authority",
            track.id
        );
        crate::now_playing::clear_loading();
        return true;
    };
    if crate::cast_qt::play_track_if_cast(track).await {
        log::info!("[play] entry={entry} track={} -> cast renderer", track.id);
        crate::now_playing::clear_loading();
        return true;
    }
    if route_play_to_peer(runtime, track.id).await {
        log::info!("[play] entry={entry} track={} -> qconnect peer", track.id);
        return true;
    }
    log::debug!("[play] entry={entry} track={} -> local", track.id);
    false
}

async fn route_play_to_peer(runtime: &Arc<AppRuntime<LoggingAdapter>>, track_id: u64) -> bool {
    let Some(svc) = crate::qconnect_qt::service() else {
        return false;
    };
    if !svc.play_on_peer_if_active(track_id).await {
        return false;
    }
    crate::now_playing::clear_loading();
    refresh_now_playing(runtime).await;
    publish_queue(runtime).await;
    true
}

/// Tripwire: a track with NEITHER a container origin NOR an album id publishes
/// `ctx-id == ""` (refresh_now_playing's fallback has nothing to fall back to),
/// which renders the song-card layers glyph as a dead control. Nothing in the
/// current producer set can hit this — the point is that the NEXT producer
/// someone adds says so in the log instead of shipping an inert button.
fn warn_dead_context(tracks: &[QueueTrack], site: &str) {
    let dead = tracks
        .iter()
        .filter(|t| {
            t.context_id.as_deref().unwrap_or("").is_empty()
                && t.album_id.as_deref().unwrap_or("").is_empty()
        })
        .count();
    if dead > 0 {
        log::warn!(
            "[qbz-qt] playback context: {dead}/{} track(s) reached {site} with no context AND no \
             album id — their 'playing from' glyph will be inert",
            tracks.len(),
        );
    }
}

// ---------------------------------------------------------------------------
// Play an album (album-card click on Home)
// ---------------------------------------------------------------------------

/// Fetch the album, build the queue (playback.rs `make_queue_track` port),
/// set it starting at track 1, and play through the core's resolved path.
/// `pub(crate)` so `library_bulk` can build ONE concatenated queue out of a
/// multi-album selection and hand it to `enqueue_track_list_mode` — going
/// through `enqueue_album` per id would publish the queue N times and, for
/// "later", append instead of landing in the manual block's tail.
pub(crate) async fn fetch_album_queue(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    album_id: &str,
) -> Result<Vec<QueueTrack>, String> {
    let album = match runtime.core().get_album(album_id).await {
        Ok(album) => album,
        Err(catalog_error) => {
            // An offline album card already has every playable byte and enough
            // metadata in index.db/library.db to build a queue. Requiring
            // /album/get before consulting that index made its overlay Play a
            // network button in disguise; AlbumView appeared to work only
            // because its document had already materialized the tracks.
            match offline_album_queue(album_id).await {
                Ok(tracks) if !tracks.is_empty() => {
                    log::info!(
                        "[qbz-qt] play_album {album_id}: catalog unavailable; using {} offline row(s)",
                        tracks.len()
                    );
                    return Ok(tracks);
                }
                Ok(_) => {}
                Err(e) => {
                    log::debug!("[qbz-qt] play_album {album_id}: offline fallback unavailable: {e}")
                }
            }
            return Err(format!("get_album {album_id} failed: {catalog_error}"));
        }
    };

    let album_title = album.title.clone();
    let album_artist = album.artist.name.clone();
    // Queue rows / the np bar feed render <= 150px but take the full
    // variant (best()) — the thumbnail down-tier was reverted after the
    // 2026-08-15 owner smoke ("mala calidad" on the big tiles). The full
    // variant set is registered so the immersive large-art feed can
    // re-resolve per slot size (D2).
    let album_artwork = album.image.best().cloned().unwrap_or_default();
    crate::artwork_qt::note_np_variants(album_id, &album.image);
    let raw_tracks = album
        .tracks
        .as_ref()
        .map(|container| container.items.as_slice())
        .unwrap_or_default();
    if raw_tracks.is_empty() {
        return Err(format!("album {album_id} has no playable tracks"));
    }
    let tracks: Vec<QueueTrack> = raw_tracks
        .iter()
        .map(|track| QueueTrack {
            id: track.id,
            title: track.title.clone(),
            version: track.version.clone(),
            artist: track
                .performer
                .as_ref()
                .map(|p| p.name.clone())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| album_artist.clone()),
            album: album_title.clone(),
            album_version: album
                .version
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string),
            duration_secs: track.duration as u64,
            artwork_url: if album_artwork.is_empty() {
                None
            } else {
                Some(album_artwork.clone())
            },
            hires: track.hires,
            bit_depth: track.maximum_bit_depth,
            sample_rate: track.maximum_sampling_rate,
            is_local: false,
            album_id: Some(album.id.clone()),
            artist_id: track.performer.as_ref().map(|p| p.id),
            // §3.1: absence is resolved in ONE place. `is_streamable()` reads a
            // missing key as AVAILABLE — only an explicit `false` marks a pull.
            streamable: track.is_streamable(),
            source: Some("qobuz".to_string()),
            parental_warning: track.parental_warning,
            source_item_id_hint: Some(album.id.clone()),
            context_kind: Some("album".to_string()),
            context_id: Some(album.id.clone()),
            isrc: track.isrc.clone(),
            recording_mbid: None,
        })
        .collect();
    Ok(tracks)
}

/// Build a Qobuz album queue entirely from the user's offline index.
///
/// The encrypted CMAF bundle remains owned by `play_resolved_offline_aware`;
/// this function only reconstructs queue metadata. `library.db` enriches the
/// index with disc/track order when available. Pre-migration rows that have no
/// library twin still play in a deterministic created/id order rather than
/// making the whole album unavailable.
async fn offline_album_queue(album_id: &str) -> Result<Vec<QueueTrack>, String> {
    use qbz_offline_cache::OfflineCacheStatus;
    use std::collections::HashMap;

    let off = crate::offline_qt::get()
        .await
        .ok_or_else(|| "offline cache is not active".to_string())?;
    let cache_path = off.get_cache_path();
    let mut cached = {
        let db = off.db.lock().await;
        db.as_ref()
            .ok_or_else(|| "offline index is not open".to_string())?
            .get_album_tracks(album_id)?
    };
    cached.retain(|track| track.status == OfflineCacheStatus::Ready);
    if cached.is_empty() {
        return Err("album has no ready offline tracks".to_string());
    }

    let library_rows: HashMap<u64, qbz_library::LocalTrack> = {
        let db = off.library_db.lock().await;
        db.as_ref()
            .and_then(|db| db.get_qobuz_download_tracks().ok())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|row| row.qobuz_track_id.map(|id| (id as u64, row)))
            .collect()
    };
    cached.sort_by_key(|track| {
        let local = library_rows.get(&track.track_id);
        (
            local.and_then(|row| row.disc_number).unwrap_or(u32::MAX),
            local.and_then(|row| row.track_number).unwrap_or(u32::MAX),
            track.created_at.clone(),
            track.track_id,
        )
    });

    Ok(cached
        .into_iter()
        .map(|cached| {
            let local = library_rows.get(&cached.track_id);
            let artwork = local
                .and_then(|row| row.artwork_path.clone())
                .or_else(|| cached.resolve_cover_path(&cache_path))
                .map(|path| qbz_models::fs_url::file_url(&path));
            let album = cached.album.clone().unwrap_or_default();
            let sample_rate = cached
                .sample_rate
                .or_else(|| local.map(|row| row.sample_rate).filter(|rate| *rate > 0.0));
            let bit_depth = cached
                .bit_depth
                .or_else(|| local.and_then(|row| row.bit_depth));
            QueueTrack {
                id: cached.track_id,
                title: cached.title,
                version: None,
                artist: cached.artist,
                album,
                album_version: None,
                duration_secs: cached.duration_secs,
                artwork_url: artwork,
                hires: bit_depth.is_some_and(|bits| bits >= 24),
                bit_depth,
                sample_rate,
                // This row bypasses the Qobuz network tier but is explicitly
                // excluded from the file-source resolver by its Offline badge.
                is_local: true,
                album_id: Some(album_id.to_string()),
                artist_id: None,
                streamable: true,
                source: Some("qobuz_download".to_string()),
                parental_warning: false,
                source_item_id_hint: local.map(|row| row.id.to_string()),
                context_kind: Some("album".to_string()),
                context_id: Some(album_id.to_string()),
                isrc: None,
                recording_mbid: None,
            }
        })
        .collect())
}

/// The /artist/page top-tracks -> QueueTrack mapping shared by `play_artist`
/// and the immersive search's (artist, next|queue) dispatch (contract
/// 2026-08-02-immersive-port §3.4: `enqueue_artist_top_by_id` = get_artist_page
/// -> top_tracks mapped like play_artist -> enqueue_track_list_mode).
/// make_top_track_queue semantics: /artist/page tracks carry a thinner
/// audio_info than /album/get tracks; fields fall back.
fn artist_top_queue_tracks(
    top_tracks: &[PageArtistTrack],
    artist_id: &str,
    artist_name: &str,
) -> Vec<QueueTrack> {
    top_tracks
        .iter()
        .map(|track| {
            let audio = track.audio_info.as_ref();
            let album = track.album.as_ref();
            let album_id = album.map(|a| a.id.clone());
            // Queue rows / the np bar feed render <= 150px but take the
            // full variant (best()) — the thumbnail down-tier was reverted
            // after the 2026-08-15 owner smoke; register the full set so the
            // immersive large-art feed can re-resolve (D2).
            if let (Some(id), Some(img)) =
                (album_id.as_deref(), album.and_then(|a| a.image.as_ref()))
            {
                crate::artwork_qt::note_np_variants(id, img);
            }
            QueueTrack {
                id: track.id,
                title: track.title.clone(),
                version: track.version.clone(),
                artist: track
                    .artist
                    .as_ref()
                    .map(|a| a.name.display.clone())
                    .filter(|n| !n.is_empty())
                    .unwrap_or_else(|| artist_name.to_string()),
                album: album.map(|a| a.title.clone()).unwrap_or_default(),
                album_version: None,
                duration_secs: track.duration.unwrap_or(0) as u64,
                artwork_url: album
                    .and_then(|a| a.image.as_ref())
                    .and_then(|img| img.best().cloned()),
                hires: audio
                    .and_then(|a| a.maximum_bit_depth)
                    .map(|b| b > 16)
                    .unwrap_or(false),
                bit_depth: audio.and_then(|a| a.maximum_bit_depth),
                sample_rate: audio.and_then(|a| a.maximum_sampling_rate),
                is_local: false,
                album_id: album_id.clone(),
                artist_id: track.artist.as_ref().map(|a| a.id),
                streamable: track
                    .rights
                    .as_ref()
                    .and_then(|r| r.streamable)
                    .unwrap_or(true),
                source: Some("qobuz".to_string()),
                parental_warning: track.parental_warning.unwrap_or(false),
                source_item_id_hint: album_id,
                context_kind: Some("artist".to_string()),
                context_id: Some(artist_id.to_string()),
                isrc: None,
                recording_mbid: None,
            }
        })
        .collect()
}

/// Artist-card play (ArtistGridCard overlay / menu, phase 16) — playback.rs
/// `play_artist` 1:1: fetch the artist page ONCE, play the Popular tracks;
/// when the artist has none, fall back to the STUDIO discography (release
/// buckets album/epSingle/ep/single in page order, deduped), concatenating
/// each album's tracks and skipping albums that fail (a bulk play must not
/// abort on one unavailable album).
pub async fn play_artist(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    artist_id: &str,
) -> Result<(), String> {
    const STUDIO_TYPES: &[&str] = &["album", "epSingle", "ep", "single"];
    let id: u64 = artist_id
        .parse()
        .map_err(|_| format!("invalid artist id: {artist_id}"))?;
    let page = runtime
        .core()
        .get_artist_page(id, None)
        .await
        .map_err(|e| format!("get_artist_page {artist_id} failed: {e}"))?;
    let artist_name = page.name.display.clone();

    // 1) Popular tracks — the primary behavior.
    let top: Vec<QueueTrack> = artist_top_queue_tracks(
        page.top_tracks.as_deref().unwrap_or(&[]),
        artist_id,
        &artist_name,
    );
    if !top.is_empty() {
        let Some(_owner_action) = begin_owner_action() else {
            return Ok(());
        };
        // F1: the anchor comes back from the seam, never from `top` — the
        // filter may have dropped `top[0]`.
        let Some(anchor) =
            set_queue_stamped(runtime, top, Some(0), PlayContext::artist(artist_id)).await
        else {
            // Every popular track was filtered. The seam already toasted the
            // count; falling through to the discography below is not possible
            // (`top` is consumed by the seam) and is not worth restructuring
            // for a case that means the artist's whole top list is dead.
            log::info!("[qbz-qt] play_artist {artist_id}: every popular track was filtered");
            return Ok(());
        };
        let first_id = anchor.track_id;
        publish_queue(runtime).await;
        // QConnect CONTROLLER mode (§7): a peer owns playback — route the play
        // to it AFTER the funnel, never run the local audible step.
        if route_play_remote(runtime, first_id, "play_artist").await {
            return Ok(());
        }
        play_resolved_offline_aware(runtime, first_id, 0)
            .await
            .map_err(|e| format!("play_track {first_id} failed: {e}"))?;
        refresh_now_playing(runtime).await;
        return Ok(());
    }

    // 2) Fallback — the studio discography (deduped album ids in the page's
    // section order; compilation/live/other omitted — studio releases only).
    let mut album_ids: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for group in page.releases.unwrap_or_default() {
        if STUDIO_TYPES.contains(&group.release_type.as_str()) {
            for item in group.items {
                if seen.insert(item.id.clone()) {
                    album_ids.push(item.id);
                }
            }
        }
    }
    if album_ids.is_empty() {
        return Err(format!(
            "artist {artist_id} has no top tracks and no studio releases"
        ));
    }
    let mut queue: Vec<QueueTrack> = Vec::new();
    for aid in &album_ids {
        match fetch_album_queue(runtime, aid).await {
            Ok(tracks) => queue.extend(tracks),
            Err(e) => log::warn!("[qbz-qt] artist-play: album {aid} skipped: {e}"),
        }
    }
    if queue.is_empty() {
        return Err(format!(
            "artist {artist_id} studio discography produced no playable tracks"
        ));
    }
    log::info!(
        "[qbz-qt] artist-play {artist_id}: discography fallback, {} tracks",
        queue.len()
    );
    // The discography queue arrives pre-stamped per ALBUM (fetch_album_queue);
    // the artist origin is what the user launched, so it wins here — the
    // explicit context overrides the per-album stamp for the whole queue.
    for track in queue.iter_mut() {
        track.context_kind = None;
        track.context_id = None;
    }
    let Some(_owner_action) = begin_owner_action() else {
        return Ok(());
    };
    // F1: the anchor is read off the queue the core received.
    let Some(anchor) =
        set_queue_stamped(runtime, queue, Some(0), PlayContext::artist(artist_id)).await
    else {
        return Err(format!(
            "artist {artist_id} discography resolved to no playable tracks"
        ));
    };
    let first_id = anchor.track_id;
    publish_queue(runtime).await;
    // QConnect CONTROLLER mode (§7): route the play to the peer (after the
    // funnel, before the local audible step).
    if route_play_remote(runtime, first_id, "play_artist").await {
        return Ok(());
    }
    play_resolved_offline_aware(runtime, first_id, 0)
        .await
        .map_err(|e| format!("play_track {first_id} failed: {e}"))?;
    refresh_now_playing(runtime).await;
    Ok(())
}

/// LOCAL/Plex album keys must NEVER reach `fetch_album_queue` — `get_album`
/// 404s on a group key, the play returns Err and the card does nothing.
/// Mirrors the Slint guard inside `("album","play")` (qbz/src/main.rs:11871,
/// helper `is_local_album_key` at :2630-2632), which exists precisely because
/// Home "Recently played" / Pinned / Most-Played rails mix local rows into a
/// Qobuz card. Every album entry point below tests this FIRST.
fn is_local_album(album_id: &str) -> bool {
    crate::library_qt::is_local_album_key(album_id)
}

/// "Play next" / "Play later" / "Add to queue" for an album (AlbumCard ⋯
/// menu): resolve the album's tracks and insert them after the current track
/// (mode "next"), land them on the manual block's tail (mode "later", #442),
/// or append them (anything else).
pub async fn enqueue_album(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    album_id: &str,
    mode: &str,
) -> Result<(), String> {
    if is_local_album(album_id) {
        log::info!("[qbz-qt] enqueue_album {album_id}: local/Plex group key -> local path");
        crate::local_playback::enqueue(
            runtime,
            "album".to_string(),
            album_id.to_string(),
            mode.to_string(),
        )
        .await;
        return Ok(());
    }
    let tracks = stamped(
        fetch_album_queue(runtime, album_id).await?,
        PlayContext::album(album_id),
    );
    // Empty after the seam: nothing to route, nothing to insert. The seam has
    // already toasted how many rows it dropped.
    if tracks.is_empty() {
        log::info!("[qbz-qt] enqueue_album {album_id}: every track was filtered");
        return Ok(());
    }
    log::info!(
        "[qbz-qt] enqueue_album {album_id} ({mode}): {} tracks",
        tracks.len()
    );
    // QConnect CONTROLLER mode (contract §7): route the add to the peer's
    // queue — early-returns when handled, so the local insert + sync tail
    // below only run in local/renderer mode. The mode is normalized to what
    // THIS fn's local match does: "next" inserts at the cursor, "later"
    // lands on the manual block's tail, anything else appends.
    let routed_mode = match mode {
        "next" => "next",
        "later" => "later",
        _ => "queue",
    };
    if route_enqueue_to_peer(&tracks, routed_mode).await {
        return Ok(());
    }
    let Some(_owner_action) = begin_owner_action() else {
        return Ok(());
    };
    let added_castable = batch_all_qconnect_castable(&tracks);
    match mode {
        "next" => {
            // add_track_next inserts directly after the current track — feed
            // in reverse so the album's first track lands first.
            for track in tracks.into_iter().rev() {
                runtime.core().add_track_next(track).await;
            }
        }
        "later" => {
            // #442 block tail — same arm the track menus and
            // enqueue_track_list_mode use, in album order.
            for track in tracks {
                runtime.core().add_track_later(track).await;
            }
        }
        _ => runtime.core().add_tracks(tracks).await,
    }
    // QConnect sync-on-add (#442): push the updated queue to the session so a
    // connected controller sees the surviving items (self-gating when a peer
    // owns playback).
    sync_qconnect_after_add(added_castable).await;
    publish_queue(runtime).await;
    Ok(())
}

pub async fn play_album(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    album_id: &str,
) -> Result<(), String> {
    play_album_from(runtime, album_id, 0).await
}

/// An album SECTION's "Play all" / "Play selected" (artist page split
/// button): every album's tracks, in the order given, as ONE queue that
/// replaces the current one — track 1 of the first album starts. Each album
/// resolves through `fetch_album_queue` like a single-album play, so a
/// purchase preference or a withdrawn row is treated identically; an album
/// that fails to resolve is logged and skipped rather than sinking the whole
/// section. Local / Plex group keys are not a catalog section and are
/// skipped.
pub async fn play_albums(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    album_ids: &[String],
) -> Result<(), String> {
    let mut queue: Vec<QueueTrack> = Vec::new();
    for album_id in album_ids {
        if is_local_album(album_id) {
            log::info!("[qbz-qt] play_albums: skipping local group key {album_id}");
            continue;
        }
        match fetch_album_queue(runtime, album_id).await {
            Ok(mut tracks) => queue.append(&mut tracks),
            Err(e) => log::warn!("[qbz-qt] play_albums: {album_id} skipped: {e}"),
        }
    }
    if queue.is_empty() {
        return Err(format!(
            "none of the {} album(s) resolved to playable tracks",
            album_ids.len()
        ));
    }
    log::info!(
        "[qbz-qt] play_albums: queueing {} track(s) from {} album(s)",
        queue.len(),
        album_ids.len()
    );
    play_track_list(runtime, queue, 0, false).await
}

/// Play an album starting at track `start_index` (AlbumView row play —
/// Slint's play_album_from semantics: the queue keeps the whole album,
/// the cursor starts at the clicked track).
pub async fn play_album_from(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    album_id: &str,
    start_index: usize,
) -> Result<(), String> {
    if is_local_album(album_id) {
        // The local path owns its own queue building + audible step. The start
        // INDEX is not expressible there (it takes a row id) — every caller
        // that reaches here does so with 0 (`play_album`), and the row-anchored
        // variant is `play_album_from_track` below, which does pass the id.
        log::info!("[qbz-qt] play_album {album_id}: local/Plex group key -> local path");
        crate::local_playback::play_album(runtime, album_id.to_string(), None, false).await;
        return Ok(());
    }
    log::info!("[qbz-qt] play_album: resolving {album_id} (start {start_index})");
    let tracks = fetch_album_queue(runtime, album_id).await?;
    let start = start_index.min(tracks.len() - 1);
    let count = tracks.len();
    let Some(_owner_action) = begin_owner_action() else {
        return Ok(());
    };
    // F1: the id to play is whatever SURVIVED at the remapped index. Reading
    // `tracks[start].id` here and playing it after the seam is the defect the
    // owner's acceptance test 7 catches — "play the album and skip the dead
    // track" used to do nothing at all, silently, because the play call was
    // handed an id the core no longer held.
    let Some(anchor) =
        set_queue_stamped(runtime, tracks, Some(start), PlayContext::album(album_id)).await
    else {
        log::info!("[qbz-qt] play_album {album_id}: every track was filtered, queue untouched");
        return Ok(());
    };
    let first_id = anchor.track_id;
    publish_queue(runtime).await;
    log::info!("[qbz-qt] play_album: queue set ({count} tracks), playing track {first_id}");
    // QConnect CONTROLLER mode (§7): route the play to the peer (after the
    // funnel, before the local audible step).
    if route_play_remote(runtime, first_id, "play_album_from").await {
        return Ok(());
    }
    play_resolved_offline_aware(runtime, first_id, 0)
        .await
        .map_err(|e| format!("play_track {first_id} failed: {e}"))?;
    log::info!("[qbz-qt] play_album: play_track_resolved started for {first_id}");
    refresh_now_playing(runtime).await;
    Ok(())
}

/// AlbumView row play: play the album starting AT the clicked track
/// (Slint play_album_from: the queue keeps the whole album).
pub async fn play_album_from_track(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    album_id: &str,
    track_id: u64,
) -> Result<(), String> {
    if is_local_album(album_id) {
        log::info!("[qbz-qt] play_album_from_track {album_id}: local/Plex group key -> local path");
        crate::local_playback::play_album(
            runtime,
            album_id.to_string(),
            Some(track_id as i64),
            false,
        )
        .await;
        return Ok(());
    }
    let tracks = fetch_album_queue(runtime, album_id).await?;
    let start = tracks.iter().position(|t| t.id == track_id).unwrap_or(0);
    let Some(_owner_action) = begin_owner_action() else {
        return Ok(());
    };
    // F1: the anchor comes from the seam. Clicking the album's DEAD row lands
    // on the next survivor rather than playing an id the core dropped.
    let Some(anchor) =
        set_queue_stamped(runtime, tracks, Some(start), PlayContext::album(album_id)).await
    else {
        log::info!(
            "[qbz-qt] play_album_from_track {album_id}: every track was filtered, queue untouched"
        );
        return Ok(());
    };
    let first_id = anchor.track_id;
    publish_queue(runtime).await;
    // QConnect CONTROLLER mode (§7): route the play to the peer (after the
    // funnel, before the local audible step).
    if route_play_remote(runtime, first_id, "play_album_from_track").await {
        return Ok(());
    }
    play_resolved_offline_aware(runtime, first_id, 0)
        .await
        .map_err(|e| format!("play_track {first_id} failed: {e}"))?;
    refresh_now_playing(runtime).await;
    Ok(())
}

/// The reference's shuffle: a Fisher–Yates pass driven by an xorshift64 PRNG
/// seeded from the wall clock (`playback.rs:3131-3144`, byte-for-byte the same
/// mix as `play_album_shuffled` / `play_artist_top_shuffled`). Split from the
/// seed so the permutation is unit-testable — the seedless wrapper is what the
/// play paths call.
fn xorshift_shuffle_seeded<T>(items: &mut [T], seed: u64) {
    // The reference ORs the seed with 1 at construction: xorshift64 is stuck
    // on zero forever, and a nanosecond clock can land there.
    let mut seed = seed | 1;
    for i in (1..items.len()).rev() {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let j = (seed % (i as u64 + 1)) as usize;
        items.swap(i, j);
    }
}

/// `xorshift_shuffle_seeded` with the reference's wall-clock seed.
///
/// `pub(crate)` so every Shuffle entry point can reorder its OWN list instead
/// of only raising the global shuffle MODE. Owner ruling 2026-08-01: "todos los
/// shuffles deben ser totalmente random"; a path that raises the flag and then
/// plays the list's #1 track is not shuffled, it is the original order with a
/// toggle lit — an artifact of a very early Tauri experiment.
pub(crate) fn xorshift_shuffle<T>(items: &mut [T]) {
    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(1);
    xorshift_shuffle_seeded(items, seed);
}

/// The toast text for a failed audio stream (`playback.rs:5102-5113`).
/// `sandboxed` is the caller's `detect_sandbox() != Sandbox::None`: inside a
/// Flatpak/Snap sandbox direct `hw:` access is blocked by design, so an
/// ALSA-shaped failure is rewritten into the actionable sentence instead of
/// the raw engine error. Pure, so the rewrite rule is unit-tested.
fn stream_error_text(msg: &str, sandboxed: bool) -> String {
    let lower = msg.to_lowercase();
    if sandboxed && (lower.contains("alsa") || lower.contains("hw:")) {
        qbz_i18n::t(
            "Direct ALSA device access is blocked inside Flatpak/Snap — switch the audio backend to PipeWire or System Default",
        )
    } else {
        format!("{}: {}", qbz_i18n::t("Audio output error"), msg)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PlaybackRecoveryCause {
    SourceFailure,
    EngineEmpty,
    SampledNaturalEnd,
}

/// Resolve one queue-recovery edge. Explicit player generations win over the
/// legacy sampled predicate: a 100 ms late-gapless resume/empty pulse can fit
/// entirely between this frontend's 1 s polls, while the generations remain
/// observable until the next tick. Track-id checks reject a stale feeder/end
/// edge after a manual play has already moved the queue.
fn playback_recovery_cause(
    event: &PlaybackEvent,
    queue_track_id: u64,
    seen_engine_empty_generation: u64,
    seen_source_failure_generation: u64,
    sampled_track_end: bool,
) -> Option<PlaybackRecoveryCause> {
    if event.source_failure_generation != seen_source_failure_generation
        && event.source_failure_track_id != 0
        && event.source_failure_track_id == queue_track_id
    {
        return Some(PlaybackRecoveryCause::SourceFailure);
    }
    if event.engine_empty_generation != seen_engine_empty_generation
        && event.engine_empty_track_id != 0
        && event.engine_empty_track_id == queue_track_id
        && !event.is_playing
        && event.gapless_next_track_id == 0
    {
        return Some(PlaybackRecoveryCause::EngineEmpty);
    }
    sampled_track_end.then_some(PlaybackRecoveryCause::SampledNaturalEnd)
}

/// Play a pre-built queue starting at `start` (ArtistView Popular Tracks:
/// the visible list becomes the queue, anchored at the clicked track).
///
/// The origin is DERIVED from the queue when the caller has none (see
/// `stamp_context`), so this path can never publish a contextless track — the
/// artist page's Popular Tracks span many albums but one artist and resolve to
/// ("artist", artist_id) without the caller lifting a finger.
///
/// `shuffle` REORDERS THIS QUEUE — it does not switch the player's shuffle
/// mode. See `play_track_list_in`.
pub async fn play_track_list(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    tracks: Vec<QueueTrack>,
    start: usize,
    shuffle: bool,
) -> Result<(), String> {
    play_track_list_in(runtime, tracks, start, shuffle, None).await
}

/// `play_track_list` with an EXPLICIT origin — preferred whenever the caller
/// knows it (artist page, playlist, label), because it survives a queue whose
/// tracks share nothing derivable.
///
/// PARITY-DEBT #17. `shuffle` means "play THIS list in a random order", which
/// in the reference is a one-shot reorder of the Vec followed by a play from
/// index 0 — the global shuffle MODE is never touched
/// (`playback.rs:3120-3145 play_label_top_shuffled`, and the identical body in
/// `play_artist_top_shuffled`:3083-3111 / `play_album_shuffled`:3945-3957).
/// This port used to answer the flag with `core().set_shuffle(true)` +
/// `now_playing::set_shuffle(true)`, so pressing Shuffle on a label or artist
/// page silently latched the bar's shuffle toggle ON for everything played
/// afterwards, AND still started on the list's #1 track. Both callers that
/// pass `true` (`label_qt::play_top`, `main::play_artist_top`) mirror a Slint
/// entry point that reorders in place, so the flag is reordering here too.
pub async fn play_track_list_in(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    tracks: Vec<QueueTrack>,
    start: usize,
    shuffle: bool,
    context: Option<PlayContext>,
) -> Result<(), String> {
    if tracks.is_empty() {
        return Err("empty track list".to_string());
    }
    // Shuffle-play starts at the top of the SHUFFLED order (`tracks[0]` after
    // the mix, exactly as the reference does), so the caller's anchor index is
    // meaningless on this path and is dropped rather than carried.
    let (tracks, start) = if shuffle {
        let mut tracks = tracks;
        xorshift_shuffle(&mut tracks);
        (tracks, 0)
    } else {
        (tracks, start)
    };
    let start = start.min(tracks.len() - 1);
    let Some(_owner_action) = begin_owner_action() else {
        return Ok(());
    };
    // F1: the anchor comes from the seam. `Err` and not `Ok` when nothing
    // survives, deliberately: `try_infinite_refill` reads this Result to decide
    // whether a radio actually started, and an `Ok` over an empty publish would
    // make it report a chain that never played.
    let Some(anchor) = set_queue_stamped(runtime, tracks, Some(start), context).await else {
        return Err("every track in the list was filtered (blacklist / unavailable)".to_string());
    };
    let first_id = anchor.track_id;
    publish_queue(runtime).await;
    // QConnect CONTROLLER mode (§7): route the play to the peer (after the
    // funnel, before the local audible step).
    if route_play_remote(runtime, first_id, "play_track_list_in").await {
        return Ok(());
    }
    play_resolved_offline_aware(runtime, first_id, 0)
        .await
        .map_err(|e| format!("play_track {first_id} failed: {e}"))?;
    refresh_now_playing(runtime).await;
    Ok(())
}

/// When the queue is exhausted and `InfiniteRadio` autoplay is on, build a
/// smart artist radio seeded by the just-finished track and start it, replacing
/// the spent queue. Returns `true` when a radio actually started.
///
/// Port of `playback.rs::try_infinite_refill`, including the reason it does not
/// mirror Tauri literally: Tauri appends the radio and retries `next()`, which
/// cannot work here because `QueueManager::next()` has already nulled
/// `current_index` at the queue edge — the retry would replay the OLD queue
/// from index 1 instead of the radio. Starting the radio fresh reaches the
/// intended behaviour and chains: each radio's end re-seeds the next one, which
/// is what makes it infinite rather than one extra radio.
///
/// `play_track_list` runs the queue through `filter_unplayable_queue`, so a
/// radio made entirely of blocked artists — or of tracks Qobuz has pulled —
/// cannot come back as playback: the seam publishes nothing and
/// `play_track_list_in` returns `Err`, which this fn reports as "no chain".
async fn try_infinite_refill(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    seed_track_id: u64,
) -> bool {
    if !crate::queue_qt::is_infinite_play() || seed_track_id == 0 {
        return false;
    }
    let artist_id = match runtime.core().get_track(seed_track_id).await {
        Ok(track) => match track.performer.as_ref().map(|p| p.id) {
            Some(id) => id,
            None => {
                log::warn!("[qbz-qt] infinite radio: seed track {seed_track_id} has no performer");
                return false;
            }
        },
        Err(e) => {
            log::warn!("[qbz-qt] infinite radio: get_track {seed_track_id} failed: {e}");
            return false;
        }
    };
    match runtime.core().create_smart_artist_radio(artist_id).await {
        Ok(tracks) if !tracks.is_empty() => {
            let queue: Vec<QueueTrack> = tracks
                .iter()
                .map(crate::foryou_qt::to_queue_track)
                .collect();
            match play_track_list(runtime, queue, 0, false).await {
                Ok(()) => {
                    log::info!(
                        "[qbz-qt] infinite radio: chained {} track(s) from artist {artist_id}",
                        tracks.len()
                    );
                    true
                }
                Err(e) => {
                    log::warn!("[qbz-qt] infinite radio: play failed: {e}");
                    false
                }
            }
        }
        Ok(_) => false,
        Err(e) => {
            log::warn!("[qbz-qt] infinite radio: build failed: {e}");
            false
        }
    }
}

/// The three insertion modes the row/card menus use,
/// mirroring `enqueue_album`: "next" inserts at the cursor (fed REVERSED so a
/// multi-track insert keeps its order), "later" appends to the manual block's
/// tail, anything else appends.
///
/// Exists because ArtistView's ⋯ menu has BOTH "Play all next" and "Add all to
/// queue" and the port wired them to the same append (the Slint arm for
/// `next-all` is `enqueue_artist_top_selected(.., next: true)`, main.rs:15301).
/// Its append-only wrapper `enqueue_track_list` is gone with that miswiring —
/// "queue" through this funnel IS the append, and a second name for it was
/// what let the mode-less call site look correct.
pub async fn enqueue_track_list_mode(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    tracks: Vec<QueueTrack>,
    mode: &str,
) -> Result<(), String> {
    if tracks.is_empty() {
        return Err("empty track list".to_string());
    }
    let tracks = stamped(tracks, None);
    // The seam can empty a batch that was non-empty on entry (the check above
    // ran BEFORE the filter). Stop here rather than routing an empty add to a
    // peer and calling `add_tracks(vec![])`: the seam already said why.
    if tracks.is_empty() {
        log::info!("[qbz-qt] enqueue_track_list_mode: every track was filtered");
        return Ok(());
    }
    // QConnect CONTROLLER mode (contract §7): route the add to the peer's
    // queue — early-returns when handled, so the local insert + sync tail
    // below only run in local/renderer mode.
    if route_enqueue_to_peer(&tracks, mode).await {
        return Ok(());
    }
    let Some(_owner_action) = begin_owner_action() else {
        return Ok(());
    };
    let added_castable = batch_all_qconnect_castable(&tracks);
    match mode {
        "next" => {
            for track in tracks.into_iter().rev() {
                runtime.core().add_track_next(track).await;
            }
        }
        "later" => {
            for track in tracks {
                runtime.core().add_track_later(track).await;
            }
        }
        _ => runtime.core().add_tracks(tracks).await,
    }
    // QConnect sync-on-add (#442): push the updated surviving queue to the
    // session.
    sync_qconnect_after_add(added_castable).await;
    publish_queue(runtime).await;
    Ok(())
}

/// ArtistView ⋯ "Play all next" / "Add all to queue" — the whole body, so the
/// invokable in `player_bridge.rs` can call it without a `main.rs` shim (see
/// GLUE in the report).
///
/// The QConnect routed arm + sync tail live in the delegate
/// (`enqueue_track_list_mode`) — arming here too would double-fire the routed
/// send whenever a peer is active.
pub async fn enqueue_artist_top_mode(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    mode: &str,
) -> Result<(), String> {
    let (queue, _) = crate::artist_qt::top_queue(None);
    enqueue_track_list_mode(runtime, queue, mode).await
}

/// Immersive search (artist, next|queue) dispatch (contract
/// 2026-08-02-immersive-port §3.4, round-2-verified mapping): fetch the
/// artist page BY ID and enqueue its top tracks through the shared mode
/// funnel. NEW additive helper — the existing `enqueue_artist_top_mode`
/// reads the ArtistView stash (`artist_qt::top_queue`) and is BANNED from
/// immersive: there is no artist view under the overlay, so the stash would
/// be stale or empty. Unlike `play_artist` there is no discography fallback
/// — the Slint arm (`main.rs:10732-10749`) also enqueues top tracks ONLY.
pub async fn enqueue_artist_top_by_id(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    artist_id: &str,
    mode: &str,
) -> Result<(), String> {
    let id: u64 = artist_id
        .parse()
        .map_err(|_| format!("invalid artist id: {artist_id}"))?;
    let page = runtime
        .core()
        .get_artist_page(id, None)
        .await
        .map_err(|e| format!("get_artist_page {artist_id} failed: {e}"))?;
    let tracks = artist_top_queue_tracks(
        page.top_tracks.as_deref().unwrap_or(&[]),
        artist_id,
        &page.name.display,
    );
    if tracks.is_empty() {
        return Err(format!("artist {artist_id} has no top tracks"));
    }
    enqueue_track_list_mode(runtime, tracks, mode).await
}

/// One track from an album context into the queue ("Play next" / "Add to
/// queue" on an AlbumView row).
pub async fn enqueue_album_track(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    album_id: &str,
    track_id: u64,
    mode: &str,
) -> Result<(), String> {
    if is_local_album(album_id) {
        // One LOCAL row: the local enqueue resolves it by row id (the cached
        // Tracks page / open detail / library.db), so the group key is only
        // used here to decide the route.
        log::info!("[qbz-qt] enqueue_album_track {album_id}/{track_id}: local/Plex -> local path");
        crate::local_playback::enqueue(
            runtime,
            "track".to_string(),
            track_id.to_string(),
            mode.to_string(),
        )
        .await;
        return Ok(());
    }
    let tracks = fetch_album_queue(runtime, album_id).await?;
    let Some(track) = tracks.into_iter().find(|t| t.id == track_id) else {
        return Err(format!("track {track_id} not in album {album_id}"));
    };
    // QConnect CONTROLLER mode (contract §7): route the add to the peer's
    // queue — early-returns when handled.
    //
    // "later" now travels VERBATIM. It used to be collapsed into "queue" here,
    // to match a local match below that had no block-tail arm — so on the peer
    // as well as locally, "Play later" and "Add to queue" did the same thing.
    // `route_track_to_peer` has had a real "later" arm all along
    // (`play_later_on_peer_if_active`); this was the line hiding it.
    let routed_mode = match mode {
        "next" => "next",
        "later" => "later",
        _ => "queue",
    };
    if route_track_to_peer(&track, routed_mode).await {
        return Ok(());
    }
    let Some(_owner_action) = begin_owner_action() else {
        return Ok(());
    };
    let added_castable = batch_all_qconnect_castable(std::slice::from_ref(&track));
    // THREE modes, like `enqueue_track_list_mode`. This match had only two, and
    // that is why the album page hid its "Play later": with "later" falling
    // into the same plain append as "Add to queue", offering both would have
    // been two menu entries doing one thing — the rendered-and-inert defect
    // this tree refuses on principle. `add_track_later` lands on the manual
    // block's TAIL, which is the whole distinction.
    match mode {
        "next" => runtime.core().add_track_next(track).await,
        "later" => runtime.core().add_track_later(track).await,
        _ => runtime.core().add_track(track).await,
    }
    // QConnect sync-on-add (#442): push the updated queue to the session;
    // skipped silently for a non-castable add.
    sync_qconnect_after_add(added_castable).await;
    publish_queue(runtime).await;
    Ok(())
}

/// Shuffle-play an album (AlbumView header Shuffle): mix the album's own list
/// and start at the top of the mixed order.
///
/// The MODE alone was not a shuffle. `set_shuffle(true)` only randomises what
/// comes NEXT, so `play_album` still started on track 1 — pressing Shuffle on
/// an album played its opener every single time. Owner ruling 2026-08-01:
/// every shuffle must be genuinely random; the flag-and-play-from-the-top
/// shape is an artifact of a very early Tauri experiment. The mode is still
/// raised so playback stays shuffled past this album.
///
/// A LOCAL album keeps going through the local path, which owns its own queue
/// building and does the same mix inside `local_playback::play_rows`.
pub async fn play_album_shuffled(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    album_id: &str,
) -> Result<(), String> {
    let Some(_owner_action) = begin_owner_action() else {
        return Ok(());
    };
    runtime.core().set_shuffle(true).await;
    crate::now_playing::set_shuffle(true);
    if is_local_album(album_id) {
        crate::local_playback::play_album(runtime, album_id.to_string(), None, true).await;
        return Ok(());
    }
    let mut tracks = fetch_album_queue(runtime, album_id).await?;
    xorshift_shuffle(&mut tracks);
    play_track_list_in(runtime, tracks, 0, false, PlayContext::album(album_id)).await
}

/// Build the queue meta for a Library-feed track (shared by
/// play_single_track / enqueue_single_track).
fn feed_queue_track(track_id: u64) -> Result<QueueTrack, String> {
    let id = track_id.to_string();
    let item = crate::library_qt::with_library(|d| {
        d.feed
            .iter()
            .find(|i| i.kind == "track" && i.id == id)
            .cloned()
    })
    .flatten()
    .ok_or_else(|| format!("track {track_id} not in the library feed"))?;
    // A local / Plex feed row is a LOCAL queue track — file path, Plex rating
    // key, real source — and `library_qt` owns the raw-row cache that knows
    // them. Falling through would rebuild it below as a Qobuz track wearing
    // the same numeric id, which is somebody else's song (see
    // `library_qt::feed_track_to_queue`, the twin of this function).
    if item.source == "local" || item.source == "plex" {
        return crate::library_qt::feed_track_to_queue(&item)
            .ok_or_else(|| format!("local track {track_id} has no raw row cached"));
    }
    let duration_secs = {
        let mut parts = item.duration.split(':');
        parts
            .next()
            .and_then(|m| m.parse::<u64>().ok())
            .unwrap_or(0)
            * 60
            + parts
                .next()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(0)
    };
    Ok(QueueTrack {
        id: track_id,
        title: item.title.clone(),
        version: None,
        artist: item.artist.clone(),
        album: item.album.clone(),
        album_version: None,
        duration_secs,
        artwork_url: if item.image_url.is_empty() {
            None
        } else {
            Some(item.image_url.clone())
        },
        // playback.rs `make_queue_track` (:2426): the CATALOG max travels with
        // the queue track. `None` here zeroed `quality_state`'s `TRACK_MAX_*`
        // seed, so a Library-feed track play drew a bare tier on the NPB
        // AudioStamp with no "24-bit / 96 kHz" line. `library_qt::FeedItem`
        // carries the raw numbers alongside the formatted `quality_detail`.
        hires: item.quality_tier == "hires",
        bit_depth: item.bit_depth,
        sample_rate: item.sample_rate,
        is_local: false,
        album_id: if item.album_id.is_empty() {
            None
        } else {
            Some(item.album_id.clone())
        },
        artist_id: item.artist_id.parse::<u64>().ok(),
        // D5: the feed row's own answer. Twin of
        // `library_qt::feed_track_to_queue` over the SAME `FeedItem` — the two
        // must not disagree about what a row is, which is the whole reason both
        // read the same field instead of hardcoding a yes.
        streamable: !item.not_streamable,
        source: Some("qobuz".to_string()),
        parental_warning: item.explicit,
        source_item_id_hint: None,
        context_kind: None,
        context_id: None,
        isrc: None,
        recording_mbid: None,
    })
}

/// Catalog fallback for a track the Library feed does NOT hold — Search rows
/// and the search hero, Home's "Recently played tracks" rail, anything reached
/// by id from a view that keeps no full `Track`. This is the Qt port of the
/// Slint's last resort, `playback.rs::play_track_now` (:3242-3283): fetch the
/// track and build its one-row queue from the response. Context is left
/// unstamped exactly like the Slint — a bare track play falls back to its own
/// album in `refresh_now_playing`.
async fn catalog_queue_track(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    track_id: u64,
) -> Result<QueueTrack, String> {
    let track = runtime
        .core()
        .get_track(track_id)
        .await
        .map_err(|e| format!("get_track {track_id} failed: {e}"))?;
    let (album_id, album_title, album_artwork) = match track.album.as_ref() {
        Some(album) => (
            album.id.clone(),
            album.title.clone(),
            // Queue row / np bar feed: full variant (best()) — the
            // thumbnail down-tier was reverted after the 2026-08-15 owner
            // smoke (contract 04 §3);
            // the full set is registered for the immersive large feed (D2).
            album.image.best().cloned().unwrap_or_default(),
        ),
        None => (String::new(), String::new(), String::new()),
    };
    if let Some(album) = track.album.as_ref() {
        if !album.id.is_empty() {
            crate::artwork_qt::note_np_variants(&album.id, &album.image);
        }
    }
    let album_key = if album_id.is_empty() {
        None
    } else {
        Some(album_id.clone())
    };
    Ok(QueueTrack {
        id: track.id,
        title: track.title.clone(),
        version: track.version.clone(),
        artist: track
            .performer
            .as_ref()
            .map(|p| p.name.clone())
            .unwrap_or_default(),
        album: album_title,
        album_version: None,
        duration_secs: track.duration as u64,
        artwork_url: if album_artwork.is_empty() {
            None
        } else {
            Some(album_artwork)
        },
        hires: track.hires,
        bit_depth: track.maximum_bit_depth,
        sample_rate: track.maximum_sampling_rate,
        is_local: false,
        album_id: album_key.clone(),
        artist_id: track.performer.as_ref().map(|p| p.id),
        // §3.1: absence is resolved in ONE place. `is_streamable()` reads a
        // missing key as AVAILABLE — only an explicit `false` marks a pull.
        streamable: track.is_streamable(),
        source: Some("qobuz".to_string()),
        parental_warning: track.parental_warning,
        source_item_id_hint: album_key,
        context_kind: None,
        context_id: None,
        isrc: track.isrc.clone(),
        recording_mbid: None,
    })
}

/// One queue row for a bare track id: the Library feed first (free, already
/// resolved), the catalog second. Before the fallback existed, every entry
/// point outside the Library feed — search rows, the search hero, Home's
/// Recently-Played-Tracks rail — returned Err here and rendered a control that
/// did nothing.
/// `pub(crate)` since 2026-08-10: `queue_qt::insert_dragged_track` resolves a
/// dragged track id through this same path, so a drop into the queue and an
/// "Add to queue" menu item cannot disagree about what a row is.
pub(crate) async fn queue_track_for(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    track_id: u64,
) -> Result<QueueTrack, String> {
    match feed_queue_track(track_id) {
        Ok(qt) => Ok(qt),
        Err(feed_miss) => {
            log::debug!("[qbz-qt] {feed_miss}; resolving from the catalog");
            catalog_queue_track(runtime, track_id).await
        }
    }
}

/// Play a single track as a one-element queue (Library track rows, Search
/// rows/hero, Home's Recently-Played-Tracks rail). The queue meta comes from
/// the Library feed row when it has one and from `get_track` otherwise; the
/// audio resolves by id through the same `play_track_resolved` path.
pub async fn play_single_track(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    track_id: u64,
) -> Result<(), String> {
    let qt = queue_track_for(runtime, track_id).await?;
    let Some(_owner_action) = begin_owner_action() else {
        return Ok(());
    };
    // Bare single-track play: no container origin, so the derive falls to the
    // track's own album — same landing spot as the Slint fallback.
    // F1 in its smallest form: when the seam drops this one row there is
    // nothing to play, and the previous queue keeps going. `track_id` is the id
    // the CALLER asked for; the anchor is the id the core actually holds.
    let Some(anchor) = set_queue_stamped(runtime, vec![qt], Some(0), None).await else {
        log::info!("[qbz-qt] play_single_track: {track_id} was filtered, queue untouched");
        return Ok(());
    };
    let first_id = anchor.track_id;
    publish_queue(runtime).await;
    log::info!("[qbz-qt] play_single_track: playing {first_id}");
    // QConnect CONTROLLER mode (§7): route the play to the peer (after the
    // funnel, before the local audible step).
    if route_play_remote(runtime, first_id, "play_single_track").await {
        return Ok(());
    }
    play_resolved_offline_aware(runtime, first_id, 0)
        .await
        .map_err(|e| format!("play_track {first_id} failed: {e}"))?;
    refresh_now_playing(runtime).await;
    Ok(())
}

/// One Library-feed track into the EXISTING queue (the TrackCard / list-row
/// context menus): "next" -> add_track_next, "later" -> add_track_later
/// (#442 block tail), "queue" -> add_track (append).
pub async fn enqueue_single_track(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    track_id: u64,
    mode: &str,
) -> Result<(), String> {
    // NOT `.expect("one track in, one track out")` any more: since the enqueue
    // seam filters (F2 + D5), one row in can legitimately be ZERO rows out — a
    // blocked artist, or a track Qobuz pulled that we hold no download for. The
    // `expect` would have turned that into a PANIC on a menu click. The seam has
    // already toasted the reason, so this is a quiet Ok.
    let Some(qt) = stamped(vec![queue_track_for(runtime, track_id).await?], None).pop() else {
        log::info!("[qbz-qt] enqueue_single_track: {track_id} was filtered out");
        return Ok(());
    };
    // QConnect CONTROLLER mode (contract §7): route the add to the peer's
    // queue — early-returns when handled, so the local insert + sync tail
    // below only run in local/renderer mode.
    if route_track_to_peer(&qt, mode).await {
        return Ok(());
    }
    let Some(_owner_action) = begin_owner_action() else {
        return Ok(());
    };
    let added_castable = batch_all_qconnect_castable(std::slice::from_ref(&qt));
    match mode {
        "next" => runtime.core().add_track_next(qt).await,
        "later" => runtime.core().add_track_later(qt).await,
        _ => runtime.core().add_track(qt).await,
    }
    // QConnect sync-on-add (#442): push the updated queue to the session;
    // skipped silently for a non-castable add.
    sync_qconnect_after_add(added_castable).await;
    publish_queue(runtime).await;
    Ok(())
}

// ---------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------

// Repeated Play must not cancel a pending start; Pause must cancel it and
// must never enter the cold-start/resume branch of the transport ladder.
fn ignore_playing_request(requested: Option<bool>, playing: bool, pending: bool) -> bool {
    match requested {
        Some(true) => playing || pending,
        Some(false) => !playing && !pending,
        None => false,
    }
}

pub async fn toggle_play(runtime: &Arc<AppRuntime<LoggingAdapter>>) {
    request_playing(runtime, None).await;
}

pub(crate) async fn request_playing(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    requested: Option<bool>,
) {
    let Some(_transport_action) = begin_transport_action() else {
        return;
    };
    // A connected renderer owns transport — the local player is stopped.
    match crate::cast_qt::service().request_playing_if_cast(requested).await {
        Ok(true) => return,
        Ok(false) => {}
        Err(e) => {
            log::warn!("[qbz-qt][Cast] toggle-play failed: {e}");
            return;
        }
    }
    // QConnect CONTROLLER mode (dispatch priority cast → QConnect → local,
    // Slint main.rs:14238-14247): a peer renderer owns playback — toggle IT.
    // `true` = handled remotely; the local path MUST NOT run. On error there
    // is no local fallback (a peer owns audio; a local resume would
    // double-play).
    if let Some(svc) = crate::qconnect_qt::service() {
        match svc.request_remote_renderer_playback_if_active(requested).await {
            Ok(true) => return,
            Ok(false) => {}
            Err(e) => {
                log::warn!("[qbz-qt][QConnect] toggle-play handoff: {e}");
                return;
            }
        }
    }
    let event = runtime.core().player().get_playback_event();
    let was_playing = event.is_playing;
    let pending = PENDING_PLAY_ID.load(Ordering::Relaxed);
    if ignore_playing_request(requested, was_playing, pending != 0) {
        return;
    }

    // COLD CURSOR: not playing, and the engine holds no loaded stream.
    //
    // `resume()` only means "un-pause a stream that is already decoded". The
    // audio thread answers a resume with no data by logging
    // "cannot resume - no audio data available" and RETURNING
    // (`qbz-player/src/player/mod.rs` around :3184-3205) — it does not error,
    // so the `Err` arm below never fires and the user just sees a dead Play
    // button. Reachable states:
    //
    //  - **A restored session.** `session_persist::restore` rebuilds the queue
    //    and the cursor, and the NPB paints the track, but nothing is loaded
    //    until something plays. This is the owner-reported bug: queue restored,
    //    bar correct, Play does nothing.
    //  - A QConnect renderer queue whose cursor sits on a never-loaded track.
    //  - A cold cursor after the queue ended.
    //
    // The reference solves it the same way and names the same log line
    // (`crates/qbz/src/playback.rs:4781-4791`, `:4826-4841`): load and play the
    // CURRENT queue track instead of resuming. A normal pause leaves the stream
    // loaded, so `has_loaded_audio()` stays true and the ordinary pause/resume
    // path is untouched.
    // A cold start is already in flight: this press is a CANCEL, not a second
    // start. Without this arm the window below is re-entrant — see
    // PENDING_PLAY_ID. Ordered before the `has_loaded_audio` test because
    // during that window the engine answers false to both.
    if !was_playing && pending != 0 {
        let Some(_owner_action) = begin_owner_action() else {
            return;
        };
        log::info!("[qbz-qt] toggle-play: cancelling the in-flight cold start of track {pending}");
        let _ = begin_resolved_play();
        PENDING_PLAY_ID.store(0, Ordering::Relaxed);
        crate::now_playing::clear_loading();
        if let Err(e) = runtime.core().stop() {
            log::warn!("[qbz-qt] toggle-play: stop-during-load failed: {e}");
        }
        return;
    }

    if !was_playing {
        let current = runtime.core().current_track().await;

        // IDLE LIST: Clear while paused intentionally removes the cursor and
        // NPB, but Add to queue / Play next / Play later deliberately do not
        // create a new current row. The paused engine can still hold the old
        // decoded stream here, so checking `has_loaded_audio()` first would
        // resume the track the user just cleared. Promote the first live queue
        // occurrence before considering resume, with the expected id checked
        // under the queue lock so a concurrent mutation cannot start a
        // different row.
        if current.is_none() {
            let Some(_owner_action) = begin_owner_action() else {
                return;
            };
            let state = runtime.core().get_queue_state_full().await;
            let Some(candidate) = state.upcoming.first().cloned() else {
                log::info!("[qbz-qt] toggle-play ignored: empty queue");
                return;
            };
            if crate::local_playback::preflight_queue_track(&candidate)
                .await
                .is_err()
            {
                return;
            }
            let Some(track) = runtime
                .core()
                .play_upcoming_at_preserving_timeline(0, candidate.id)
                .await
            else {
                log::warn!("[qbz-qt] toggle-play: idle queue changed before activation");
                return;
            };
            log::info!(
                "[qbz-qt] toggle-play: idle queue -> starting first track {}",
                track.id
            );
            PENDING_PLAY_ID.store(track.id, Ordering::Relaxed);
            play_queue_track(runtime, track.id, 0).await;
            PENDING_PLAY_ID
                .compare_exchange(track.id, 0, Ordering::Relaxed, Ordering::Relaxed)
                .ok();
            return;
        }

        if !runtime.core().player().has_loaded_audio() {
            let Some(_owner_action) = begin_owner_action() else {
                return;
            };
            if let Some(track) = current {
                log::info!(
                    "[qbz-qt] toggle-play: no loaded audio -> cold-starting current track {}",
                    track.id
                );
                PENDING_PLAY_ID.store(track.id, Ordering::Relaxed);
                // Goes through the normal play path, so the persisted resume
                // position is consumed by the same `take_resume_for` handshake
                // a fresh play uses — a bare `resume()` could never have
                // applied it, because there was no stream to seek.
                play_queue_track(runtime, track.id, 0).await;
                // Cleared unconditionally: `play_queue_track` swallows its own
                // errors, so this is the only place a FAILED cold start can
                // release the latch. The poll loop's track-change edge clears
                // it too — whichever runs first wins, and both are idempotent.
                // Leaving it set on failure is precisely the Tauri scar.
                PENDING_PLAY_ID
                    .compare_exchange(track.id, 0, Ordering::Relaxed, Ordering::Relaxed)
                    .ok();
            }
            return;
        }
    }

    let result = if was_playing {
        runtime.core().pause()
    } else {
        runtime.core().resume()
    };
    if let Err(e) = result {
        log::warn!("[qbz-qt] toggle-play failed: {e}");
        return;
    }
    if was_playing {
        // Persist the paused position: pausing is the strongest signal the user
        // is done for now, and the ~5s poll flush can be up to 5s stale.
        // No-op unless `persist_session` is on.
        if let Some(_owner_action) = begin_owner_action() {
            qbz_app::session_persist::capture_and_save(runtime).await;
        }
    }
}

pub async fn next(runtime: &Arc<AppRuntime<LoggingAdapter>>) {
    let Some(_owner_action) = begin_owner_action() else {
        return;
    };
    // QConnect CONTROLLER mode (Slint main.rs:14268-14277): a peer renderer
    // owns playback — skip on IT and return; the local cursor stays put (the
    // cloud echo re-materializes the queue). NOTE: no cast branch here, 1:1
    // with the reference (main.rs:14261-14267) — while casting, the local
    // next() flow runs and the play arm casts the new current track.
    if let Some(svc) = crate::qconnect_qt::service() {
        match svc.skip_next_if_remote().await {
            Ok(true) => return,
            Ok(false) => {}
            Err(e) => {
                log::warn!("[qbz-qt][QConnect] next handoff: {e}");
                return;
            }
        }
    }
    // Prove a local file still exists before advancing the cursor. The player
    // keeps the old stream alive until a replacement starts, so advancing
    // first would make queue, NPB and MPRIS lie about what is audible.
    if let Some(candidate) = runtime.core().peek_upcoming(1).await.into_iter().next() {
        if crate::local_playback::preflight_queue_track(&candidate)
            .await
            .is_err()
        {
            return;
        }
    }
    if let Some(track) = runtime.core().next_track().await {
        play_queue_track(runtime, track.id, 0).await;
    }
}

pub async fn previous(runtime: &Arc<AppRuntime<LoggingAdapter>>) {
    let Some(_owner_action) = begin_owner_action() else {
        return;
    };
    // QConnect CONTROLLER mode (Slint main.rs:14293-14302) — see next() above.
    if let Some(svc) = crate::qconnect_qt::service() {
        match svc.skip_previous_if_remote().await {
            Ok(true) => return,
            Ok(false) => {}
            Err(e) => {
                log::warn!("[qbz-qt][QConnect] previous handoff: {e}");
                return;
            }
        }
    }
    // `previous_track` consumes history and publishes QueueUpdated. Validate
    // a physical local target first so a missing/moved file cannot leave the
    // cursor, NPB and MPRIS one row behind the audio the user still hears.
    if let Some(candidate) = runtime.core().peek_previous_track().await {
        if crate::local_playback::preflight_queue_track(&candidate)
            .await
            .is_err()
        {
            return;
        }
    }
    if let Some(track) = runtime.core().previous_track().await {
        play_queue_track(runtime, track.id, 0).await;
    }
}

pub(crate) async fn play_queue_track_public(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    track_id: u64,
) {
    play_queue_track(runtime, track_id, 0).await;
}

/// Play a queue track from the top of the source ladder (cast → peer → local
/// → Qobuz).
///
/// `start_position_secs` is the position to START at, and 0 means "the normal
/// start", NOT "second zero": `play_resolved_offline_aware` treats 0 as the
/// signal to consult `session_persist::take_resume_for`, so a restored track
/// resumes where it was left. A non-zero value is an EXPLICIT request and wins
/// over the restore stash — which is what lets a seek on a cold cursor
/// cold-start directly at the dragged position.
async fn play_queue_track(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    track_id: u64,
    start_position_secs: u64,
) {
    // Supersede preference/filesystem work that has not reached the player
    // yet. Qobuz starts below bump once more when it enters the resolved
    // funnel; local/remote source starts still need this first fence.
    let _ = begin_resolved_play();
    // A connected renderer (cast, then QConnect peer) owns playback: route
    // the new track to it and never start the local backend, or both play.
    // Same seam as every other entry point — see `route_play_remote`.
    if route_play_remote(runtime, track_id, "play_queue_track").await {
        return;
    }
    let Some(_owner_action) = begin_owner_action() else {
        return;
    };
    // Source-aware audible step: a LOCAL file plays from disk through the
    // player's play_data seam. The Qobuz tier-walk below needs a client and
    // would fail with "No Qobuz client available" on every local advance.
    // ONE seam for both kinds of "cannot play". A local/Plex row that fails
    // must NOT fall through to the Qobuz walk below: it would fail there with
    // an unrelated error, `auto_skip_unavailable` would decline it as
    // non-terminal, and playback would stop dead on one unreachable file. To
    // the user a withdrawn Qobuz track and a file on a share that went away
    // are the same event, so they take the same bounded skip walk.
    match crate::local_library_qt::play_current_if_local(runtime, track_id).await {
        crate::local_playback::LocalPlay::Played => {
            refresh_now_playing(runtime).await;
            publish_queue(runtime).await;
            return;
        }
        crate::local_playback::LocalPlay::Unavailable(reason) => {
            log::warn!("[qbz-qt] playback: {reason} — skipping");
            refresh_now_playing(runtime).await;
            publish_queue(runtime).await;
            auto_skip_unavailable(runtime, reason, Some(track_id)).await;
            return;
        }
        crate::local_playback::LocalPlay::NotLocal => {}
    }
    if let Err(e) = play_resolved_offline_aware(runtime, track_id, start_position_secs).await {
        log::error!("[qbz-qt] playback: play_track {track_id} failed: {e}");
        // The bar must not keep showing the PREVIOUS track while the cursor has
        // already moved. The old early `return` skipped both refreshes, which is
        // how a failed advance left a stale card over a silent player.
        refresh_now_playing(runtime).await;
        publish_queue(runtime).await;
        auto_skip_unavailable(runtime, e, Some(track_id)).await;
        return;
    }
    // A track actually started: the consecutive-skip budget is spent per RUN of
    // bad tracks, not per session (playback.rs UNAVAILABLE_SKIPS reset).
    UNAVAILABLE_SKIPS.store(0, Ordering::SeqCst);
    refresh_now_playing(runtime).await;
    publish_queue(runtime).await;
}

/// Re-resolve an already audible album track after AlbumView changes its
/// persisted playback source. The queue/cursor stay intact; only the bytes are
/// replaced. The player's current position is a cheap atomic snapshot, so the
/// ordinary resolved funnel can resume there instead of making the user wait
/// for the next track (or restarting unnecessarily).
pub(crate) async fn rematerialize_current_album_source(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    album_id: &str,
) {
    let Some(owner_action) = begin_owner_action() else {
        return;
    };
    if crate::cast_qt::is_casting().await {
        return;
    }
    let Some(track) = runtime.core().current_track().await else {
        return;
    };
    let event = runtime.core().player().get_playback_event();
    if !source_change_targets_current(album_id, &track, &event) {
        return;
    }
    let position = event.position;
    if let Err(error) = runtime.core().player().stop() {
        log::warn!(
            "[qbz-qt] purchase: could not stop track {} for source change: {error}",
            track.id
        );
        return;
    }
    drop(owner_action);
    log::info!(
        "[qbz-qt] purchase: rematerializing track {} at {}s after source change",
        track.id,
        position
    );
    play_queue_track(runtime, track.id, position).await;
}

fn source_change_targets_current(
    album_id: &str,
    track: &QueueTrack,
    event: &PlaybackEvent,
) -> bool {
    event.is_playing
        && event.track_id == track.id
        && track
            .album_id
            .as_deref()
            .is_some_and(|current_album| current_album == album_id)
}

/// True when a stringified play error means the track cannot play now or ever
/// at any quality, as opposed to a transient network/server failure (those are
/// already retried with backoff inside the client and must NOT cost the user a
/// good track). Verbatim from playback.rs:327-332 — the `ApiError` Display texts
/// survive the `Result<(), String>` flattening, so a substring match is the same
/// pragmatic contract the Tauri frontend used.
fn is_terminal_unavailable(e: &str) -> bool {
    e.contains("no longer available") // ApiError::TrackUnavailable
        || e.contains("not streamable") // ApiError::NonStreamable
        || e.contains("No valid quality available") // ApiError::NoQualityAvailable
}

/// A Qobuz 403 / the client-side 403 back-off (issue #637). NOT the track's
/// fault — every track 403s the same way — so it must not count as an
/// "unavailable" skip. Pre-fix in the Slint this burned through five good
/// tracks while showing a misleading "no longer available"
/// (playback.rs:334-343).
fn is_forbidden_backoff(e: &str) -> bool {
    e.contains("Access forbidden by Qobuz") // ApiError::Forbidden
        || e.contains("backing off after repeated 403s") // ApiError::ForbiddenCircuitOpen
}

/// Consecutive auto-skips over tracks whose play failed TERMINALLY (Tauri #467
/// parity: `MAX_CONSECUTIVE_SKIPS = 5`). Reset the moment any track starts.
static UNAVAILABLE_SKIPS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
const MAX_UNAVAILABLE_SKIPS: u32 = 5;

/// One unavailable track must not wedge the whole queue (PARITY-DEBT #2 —
/// Tauri #467, re-introduced by this port). Port of `auto_skip_unavailable`
/// (playback.rs:766-818).
///
/// The signature RETURNS a boxed `dyn Future + Send` rather than being an
/// `async fn`, for the reason the reference documents: the recursion makes the
/// future's Send-ness self-referential and an inferred `impl Future` type sends
/// the compiler into a query cycle. Declaring the concrete boxed type is what
/// cuts it.
fn auto_skip_unavailable<'a>(
    runtime: &'a Arc<AppRuntime<LoggingAdapter>>,
    error: String,
    failed_track_id: Option<u64>,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + 'a>> {
    Box::pin(async move {
        // A 403 / back-off is not this track's fault: stop cleanly and say so
        // instead of eating five good tracks looking for one that works.
        if is_forbidden_backoff(&error) {
            crate::toast_qt::error(qbz_i18n::t("Access forbidden by Qobuz"));
            crate::now_playing::set_playing(false);
            return;
        }
        // Anything that is not TERMINALLY unavailable is transient — the client
        // already retried it. Do not skip a good track over a network blip.
        if !is_terminal_unavailable(&error) {
            return;
        }
        let Some(_owner_action) = begin_owner_action() else {
            return;
        };
        // §5.1 clause 2 (D3): remember it for the rest of the run, so the queue
        // filter stops offering it to the player. The API flag covers the
        // tracks Qobuz already admits are gone; THIS covers the ones it still
        // advertises as streamable and then refuses to serve — which is exactly
        // what the owner's test track does at the stream-url step.
        if let Some(id) = failed_track_id {
            crate::track_replace_qt::session_unavailable::mark(id);
        }
        crate::toast_qt::warning(qbz_i18n::t("This track is no longer available"));

        let skips = UNAVAILABLE_SKIPS.fetch_add(1, Ordering::SeqCst) + 1;
        if skips > MAX_UNAVAILABLE_SKIPS {
            log::warn!(
                "[qbz-qt] playback: {MAX_UNAVAILABLE_SKIPS} consecutive unavailable tracks — stopping the skip walk"
            );
            crate::toast_qt::warning(qbz_i18n::t("No available tracks to play"));
            crate::now_playing::set_playing(false);
            UNAVAILABLE_SKIPS.store(0, Ordering::SeqCst);
            // Nowhere left to skip TO, so clean rather than leave the user a
            // queue of rows that each stop playback when reached.
            crate::queue_qt::purge_unavailable_upcoming(runtime).await;
            return;
        }
        let Some(track) = runtime.core().next_track().await else {
            // Queue edge reached while skipping — stop quietly, the warning
            // above already told the user why. Same clean-up as the budget
            // arm: the rows behind us cannot play either.
            crate::now_playing::set_playing(false);
            UNAVAILABLE_SKIPS.store(0, Ordering::SeqCst);
            crate::queue_qt::purge_unavailable_upcoming(runtime).await;
            return;
        };
        log::info!(
            "[qbz-qt] advance: skipping unavailable track, trying {} ({skips}/{MAX_UNAVAILABLE_SKIPS})",
            track.id
        );
        play_queue_track(runtime, track.id, 0).await;
    })
}

/// The LOCAL tail of `seek_frac` (no cast / QConnect routing): the A-B loop's
/// jump, which is only ever armed for local playback.
pub(crate) async fn seek_local_secs(runtime: &Arc<AppRuntime<LoggingAdapter>>, target: u64) {
    let Some(_transport_action) = begin_transport_action() else {
        return;
    };
    if let Err(e) = runtime.core().seek(target) {
        log::warn!("[qbz-qt] seek failed: {e}");
        return;
    }
    crate::media_controls_qt::push_seeked(target);
}

pub async fn seek_frac(runtime: &Arc<AppRuntime<LoggingAdapter>>, frac: f32) {
    let Some(_transport_action) = begin_transport_action() else {
        return;
    };
    match crate::cast_qt::service()
        .seek_fraction_if_cast(frac as f64)
        .await
    {
        Ok(true) => return,
        Ok(false) => {}
        Err(e) => {
            log::warn!("[qbz-qt][Cast] seek failed: {e}");
            return;
        }
    }
    // The peer route resolves the current remote track's duration. The local
    // player can still retain the duration from before takeover (or zero).
    if let Some(svc) = crate::qconnect_qt::service() {
        match svc.seek_fraction_if_remote(frac).await {
            Ok(true) => return,
            Ok(false) => {}
            Err(e) => {
                log::warn!("[qbz-qt][QConnect] seek handoff: {e}");
                return;
            }
        }
    }
    let event = runtime.core().player().get_playback_event();

    // COLD CURSOR: the bar is draggable but the engine holds no stream.
    //
    // Two ways in, and BOTH were silently wrong before this branch:
    //
    //  (a) A restored session. `event.duration` is 0 because nothing ever
    //      loaded, so the guard below returned and the drag did nothing at
    //      all. The bar still looked seekable, because the NPB's duration
    //      comes from the restored track's META, not from the engine. The
    //      keyboard seek was worse: it derives its fraction from the MODEL
    //      position, which is non-zero, so it built a perfectly valid
    //      fraction and handed it to a function that discarded it.
    //  (b) A cold cursor after a Stop (cast/QConnect handoff, or a queue that
    //      ended). `SharedState::duration` is never reset by the Stop arm, so
    //      it still holds the PREVIOUS track's length — the guard passed,
    //      `seek()` returned Ok because it only posts a command, and we then
    //      told D-Bus we had jumped. The audio thread meanwhile bailed with
    //      "cannot seek - no audio data available". The desktop widget was
    //      being told a position the player was not at and could not reach.
    //
    // Cold-starting AT the requested position is the honest answer to a drag,
    // and it composes for free: a non-zero `start_position_secs` is treated as
    // an explicit request and wins over the restore stash
    // (`play_resolved_offline_aware`), so dragging before pressing Play does
    // what the user asked instead of jumping back to the saved position.
    if !runtime.core().player().has_loaded_audio() {
        let Some(track) = runtime.core().current_track().await else {
            return;
        };
        let duration = track.duration_secs;
        if duration == 0 {
            return;
        }
        let target = (frac.clamp(0.0, 1.0) * duration as f32) as u64;
        log::info!(
            "[qbz-qt] seek on a cold cursor -> cold-starting track {} at {target}s",
            track.id
        );
        play_queue_track(runtime, track.id, target).await;
        return;
    }

    if event.duration == 0 {
        return;
    }
    let target = (frac.clamp(0.0, 1.0) * event.duration as f32) as u64;
    crate::ab_loop_qt::on_manual_seek(target);
    if let Err(e) = runtime.core().seek(target) {
        log::warn!("[qbz-qt] seek failed: {e}");
        return;
    }
    // Tell the desktop we JUMPED. The 1 Hz `push_position` keeps the stored
    // value fresh for a client that opens later; `Seeked` is the only thing
    // that reaches one already open and extrapolating from what it read then.
    crate::media_controls_qt::push_seeked(target);
}

pub async fn set_volume(runtime: &Arc<AppRuntime<LoggingAdapter>>, volume: f32) {
    let Some(_transport_action) = begin_transport_action() else {
        return;
    };
    if matches!(
        crate::cast_qt::service().set_volume_if_cast(volume).await,
        Ok(true)
    ) {
        return;
    }
    // QConnect CONTROLLER mode (Slint main.rs:14378-14389): set the REMOTE
    // renderer's volume. The remote API wants 0..100; the bar gives a 0..1
    // fraction. TRAP (§12.30): a volume-DISALLOWING peer returns Ok(true) —
    // handled NO-OP — so a drag on a volume-locked peer never falls through
    // to change the LOCAL volume.
    if let Some(svc) = crate::qconnect_qt::service() {
        let remote_volume = (volume.clamp(0.0, 1.0) * 100.0).round() as i32;
        match svc.set_volume_if_remote(remote_volume).await {
            Ok(true) => return,
            Ok(false) => {}
            Err(e) => {
                log::warn!("[qbz-qt][QConnect] set-volume handoff: {e}");
                return;
            }
        }
    }
    let _ = runtime.core().set_volume(volume.clamp(0.0, 1.0));
    MUTED.store(
        volume <= 0.0 && PREMUTE_VOLUME.load(Ordering::Relaxed) != 0,
        Ordering::Relaxed,
    );
}

pub async fn toggle_mute(runtime: &Arc<AppRuntime<LoggingAdapter>>) {
    let Some(_transport_action) = begin_transport_action() else {
        return;
    };
    // The peer route toggles the peer's reported/optimistic mute state; the
    // local MUTED flag belongs to the owner and must survive handoff intact.
    if let Some(svc) = crate::qconnect_qt::service() {
        match svc.toggle_mute_if_remote().await {
            Ok(true) => return,
            Ok(false) => {}
            Err(e) => {
                log::warn!("[qbz-qt][QConnect] toggle-mute handoff: {e}");
                return;
            }
        }
    }
    if MUTED.swap(false, Ordering::Relaxed) {
        // Unmute — restore the stored level.
        let restored = f32::from_bits(PREMUTE_VOLUME.load(Ordering::Relaxed) as u32);
        let restored = if restored > 0.0 { restored } else { 0.7 };
        let _ = runtime.core().set_volume(restored);
        crate::now_playing::set_muted(false);
    } else {
        // Mute — stash the current level, then drop to zero.
        let current = runtime.core().player().get_playback_event().volume;
        let current = if current > 0.0 { current } else { 0.7 };
        PREMUTE_VOLUME.store(current.to_bits() as u64, Ordering::Relaxed);
        MUTED.store(true, Ordering::Relaxed);
        let _ = runtime.core().set_volume(0.0);
        crate::now_playing::set_muted(true);
    }
}

pub async fn toggle_shuffle(runtime: &Arc<AppRuntime<LoggingAdapter>>) {
    let Some(owner_action) = begin_owner_action() else {
        return;
    };
    // QConnect (Slint main.rs:14492-14501 — no cast arm in the reference):
    // the CLOUD owns shuffle order while connected (gated on
    // transport_connected inside the service — §12.14, no extra gate here);
    // QBZ applies only the echo. `true` = handled remotely — a local reorder
    // would diverge from the cloud's order.
    if let Some(svc) = crate::qconnect_qt::service() {
        match svc.toggle_shuffle_if_remote().await {
            Ok(true) => return,
            Ok(false) => {}
            Err(e) => {
                log::warn!("[qbz-qt][QConnect] toggle-shuffle handoff: {e}");
                return;
            }
        }
    }
    let event_before_reorder = runtime.core().player().get_playback_event();
    let enabled = runtime.core().toggle_shuffle().await;
    QUEUE_ORDER_GENERATION.fetch_add(1, Ordering::SeqCst);
    crate::now_playing::set_shuffle(enabled);
    publish_queue(runtime).await;

    // A successor may already be appended to the audio engine during the
    // final gapless window. Reordering only the QueueManager would then let
    // that stale edge play anyway; the next poll would sync the cursor past
    // the canonical rows we just preserved. Re-materialize the same current
    // occurrence only when the armed successor no longer matches the new
    // first upcoming row. Ordinary toggles do not touch playback.
    let upcoming_id = runtime
        .core()
        .peek_upcoming(1)
        .await
        .first()
        .map(|track| track.id);
    if gapless_successor_changed(&event_before_reorder, upcoming_id) {
        let track_id = event_before_reorder.track_id;
        let position = event_before_reorder.position;
        let was_playing = event_before_reorder.is_playing;
        if let Err(error) = runtime.core().player().stop() {
            log::warn!(
                "[qbz-qt] shuffle: could not discard stale gapless successor for track {track_id}: {error}"
            );
            return;
        }
        drop(owner_action);
        log::info!(
            "[qbz-qt] shuffle: re-materializing track {track_id} at {position}s after its armed successor changed"
        );
        play_queue_track(runtime, track_id, position).await;
        if !was_playing {
            if let Err(error) = runtime.core().pause() {
                log::warn!(
                    "[qbz-qt] shuffle: could not restore paused state for track {track_id}: {error}"
                );
            }
        }
    }
}

fn gapless_successor_changed(event: &PlaybackEvent, upcoming_id: Option<u64>) -> bool {
    event.track_id != 0
        && event.gapless_next_track_id != 0
        && Some(event.gapless_next_track_id) != upcoming_id
}

pub async fn cycle_repeat(runtime: &Arc<AppRuntime<LoggingAdapter>>) {
    let Some(_owner_action) = begin_owner_action() else {
        return;
    };
    // QConnect (Slint main.rs:14517-14526 — no cast arm in the reference):
    // gated on transport_connected inside the service (§12.14).
    if let Some(svc) = crate::qconnect_qt::service() {
        match svc.cycle_repeat_if_remote().await {
            Ok(true) => return,
            Ok(false) => {}
            Err(e) => {
                log::warn!("[qbz-qt][QConnect] cycle-repeat handoff: {e}");
                return;
            }
        }
    }
    let next_mode = match crate::now_playing::repeat_mode() {
        0 => RepeatMode::All,
        1 => RepeatMode::One,
        _ => RepeatMode::Off,
    };
    runtime.core().set_repeat_mode(next_mode).await;
    let value = match next_mode {
        RepeatMode::Off => 0,
        RepeatMode::All => 1,
        RepeatMode::One => 2,
    };
    crate::now_playing::set_repeat_mode(value);
}

// ---------------------------------------------------------------------------
// Meta + queue publishing
// ---------------------------------------------------------------------------

/// Push the queue's current-track meta into the NowPlayingModel (title /
/// artist / quality / artwork via the phase-3 pipeline).
pub(crate) async fn refresh_now_playing(runtime: &Arc<AppRuntime<LoggingAdapter>>) {
    let state = runtime.core().get_queue_state().await;
    let Some(track) = state.current_track else {
        crate::now_playing::clear_track();
        crate::player_bridge::ui(move |mut b| {
            b.as_mut().set_np_track_id(QString::from(""));
            b.as_mut().set_np_local_track_id(QString::from(""));
        });
        crate::lyrics_qt::publish_idle();
        // Tray tooltip: the no-track arm. 1:1 with the reference, where
        // `clear_track()` (crates/qbz/src/playback.rs:1918) and `set_track(…)`
        // (`:2160`) are the two arms of ONE publisher,
        // `refresh_now_playing_meta` (`:1895`) — this function is its Qt
        // analogue, and BOTH pushes live here. Silent no-op with no tray.
        // Clearing also drops `is_playing`, so the middle-click hint cannot go
        // stale.
        if let Some(t) = crate::tray_qt::handle() {
            t.clear_track();
        }
        // OS media controls: the no-track arm, a SIBLING of the tray clear
        // above and in the reference's order — dedupe reset, tray, MPRIS
        // (crates/qbz/src/playback.rs:1913-1922). Tells the Plasma/GNOME widget
        // we stopped and resets the metadata dedupe, so replaying the SAME
        // track after a stop pushes its metadata again. Silent no-op when the
        // integration never started.
        crate::media_controls_qt::clear_now_playing();
        return;
    };
    let title = match track.version.as_deref().filter(|v| !v.is_empty()) {
        Some(version) => format!("{} ({version})", track.title),
        None => track.title.clone(),
    };
    let track_id_str = track.id.to_string();
    // Active-row ALIAS for an offline row (Slint playback.rs:1978-1986). Its
    // queue id is the Qobuz catalog id — that is what the offline tier is
    // keyed on — but every Local Library list draws the row with its
    // `library.db` id, so `np_track_id` alone never matched the row the user
    // just played and the playing affordance stayed dark there.
    //
    // Restricted to Offline on purpose, and the restriction is the whole
    // safety of it: `source_item_id_hint` is a shared slot that carries a
    // Plex ALBUM key and a media server's own item id for those sources
    // (`local_queue_track`). Publishing it unconditionally would hand a row
    // list a number from another id space and light the wrong row.
    let local_track_id_str =
        if qbz_source::RawRef::from_queue_track(&track).badge == qbz_source::SourceBadge::Offline {
            track.source_item_id_hint.clone().unwrap_or_default()
        } else {
            String::new()
        };
    crate::player_bridge::ui(move |mut b| {
        b.as_mut()
            .set_np_track_id(QString::from(track_id_str.as_str()));
        b.as_mut()
            .set_np_local_track_id(QString::from(local_track_id_str.as_str()));
    });
    log::info!(
        "[qbz-qt] now playing: '{title}' — {} ({}s)",
        track.artist,
        track.duration_secs,
    );
    // Tray tooltip: the has-track arm (§5.6). Edge-triggered — this publisher
    // runs on a track change, never per tick (§7-M12).
    //
    // The three strings are exactly the reference's
    // (crates/qbz/src/playback.rs:2163-2165): the VERSIONED title, the artist,
    // and the CLEAN album — `track.album`, NOT the release-variant
    // `album_display` built below, which `playback.rs:1937` / `:1941-1948`
    // deliberately keeps as a separate value.
    //
    // It goes in THIS function and not in the poll loop's de-duped track edge:
    // `refresh_now_playing` has 23 call sites across nine modules, so binding
    // the tooltip to the poll loop would leave it stale on every QConnect
    // handoff, cast event and click-to-play (§15 trap 34).
    if let Some(t) = crate::tray_qt::handle() {
        t.set_track(title.clone(), track.artist.clone(), track.album.clone());
    }
    let purchase_format = (!track.is_local)
        .then(|| crate::purchase_playback_qt::materialized_format(track.id))
        .flatten();
    let (tier, label) = match purchase_format.and_then(purchase_quality_badge) {
        Some((tier, label, _, _)) => (tier.to_string(), label.to_string()),
        None => quality_badge(&track).await,
    };
    let album_id = track.album_id.clone().unwrap_or_default();
    // Album with its release variant appended ("Octavarium (2009 Remaster)"),
    // 1:1 with the Slint `album_display` (playback.rs:1941-1949).
    let album_display = match track
        .album_version
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        Some(v) => format!("{} ({v})", track.album),
        None => track.album.clone(),
    };
    // OS media controls: the has-track arm, the second SIBLING of the tray push
    // above (crates/qbz/src/playback.rs:2208-2223). This is what the KDE Plasma
    // now-playing widget, GNOME's media section and playerctl read.
    //
    // It sits HERE, after `album_display` is built and BEFORE `title` is moved
    // into the NowPlayingModel meta below, because MPRIS wants the pair the
    // reference sends: the VERSIONED title and the release-variant album — not
    // the clean `track.album` the tray tooltip takes (`:2213` vs `:2164`).
    //
    // Metadata is de-duped on (track id, resolved art) INSIDE the callee: this
    // publisher re-runs on resume/seek/republish from 23 call sites, so an
    // unconditional push would re-emit PropertiesChanged for identical values.
    // The playback push is unconditional and optimistic (Playing at 0) exactly
    // as in the reference — the poll loop's play/pause edge corrects it if the
    // stream never opens.
    crate::media_controls_qt::push_now_playing(&track, &title, &album_display);
    // "Playing from" origin for the song-card layers glyph, re-derived from the
    // CURRENT track's own stamp on EVERY change (never a stale global) —
    // playback.rs:1953-1965. A track with no container origin falls back to its
    // own album, exactly like the Slint.
    let (context_kind, context_id) = match (
        track.context_kind.as_deref().filter(|s| !s.is_empty()),
        track.context_id.as_deref().filter(|s| !s.is_empty()),
    ) {
        (Some(kind), Some(id)) => (kind.to_string(), id.to_string()),
        _ => ("album".to_string(), album_id.clone()),
    };
    crate::now_playing::set_track(crate::now_playing::TrackMeta {
        title: title.clone(),
        artist: track.artist.clone(),
        album: album_display.clone(),
        album_id,
        artist_id: track.artist_id.map(|id| id.to_string()).unwrap_or_default(),
        track_id: track.id,
        source: track.source.clone().unwrap_or_default(),
        context_kind,
        context_id,
        duration_secs: track.duration_secs as i32,
        quality_tier: tier,
        quality_label: label,
        artwork_url: track.artwork_url.clone().unwrap_or_default(),
        shuffle: state.shuffle,
        repeat_mode: match state.repeat {
            RepeatMode::Off => 0,
            RepeatMode::All => 1,
            RepeatMode::One => 2,
        },
    });
    // The two output LEDs (+ the volume-lock flag) are decided HERE, not when
    // the Settings page republishes: a stream is about to open and the
    // backend/mode it will use is whatever the audio settings say right now.
    // Publishing on the track edge is what makes the stamp live from the first
    // note instead of updating only when the user changes page. Cheap: one WAL
    // read + one Qt hop per track — no poll, and NOT publish_snapshot (that
    // rebuilds the entire Settings document).
    crate::output_labels::publish_current();
    // Catalog max + whether the streaming-quality pref governs this request:
    // the downgrade arrow compares DELIVERED against this (quality_state.rs).
    // Local and Plex sources are not governed — nothing downgrades them.
    let (source_depth, source_rate) = purchase_format
        .and_then(purchase_quality_badge)
        .map(|(_, _, depth, rate)| (depth, rate))
        .unwrap_or((track.bit_depth, track.sample_rate));
    let governed = !track.is_local && purchase_format.is_none();
    crate::now_playing::set_catalog_quality(source_depth, source_rate, governed);
    // Artwork through the same cache pipeline as Home (attach + background
    // download + republish — single url here).
    crate::artwork_qt::attach_now_playing(&track, &title, &album_display);
    // Ambient background triad (phase 14): recompute from the new track's
    // cover (no-op until the cover lands on disk — the previous palette
    // stays, like the Slint's default-until-resolved).
    crate::ambient_qt::update_for_artwork(&track.artwork_url.clone().unwrap_or_default());
    // Lyrics for the new track (loading state, then the doc).
    crate::lyrics_qt::publish_loading();
    let runtime = runtime.clone();
    let track = track.clone();
    crate::spawn(async move {
        crate::lyrics_qt::load_for_track(&runtime, &track).await;
    });
}

/// Quality tier + exact label from a queue track's bit depth / sample rate.
///
/// 1:1 with `playback.rs:2025-2042` (`refresh_now_playing_meta`), which this
/// had forked from in three ways, each of them visible on the NPB AudioStamp:
///
///  - **DSD.** `bit_depth == Some(1)` marks a DSD stream (1-bit, `sample_rate`
///    = the DSD bit rate). It tiers "dsd" (its own mark since 2026-09-02; the
///    reference said "hires") and labels it through `dsd_multiple_label`
///    ("DSD64"); the forked version tiered it
///    "cd" and printed "1-bit / 2822.4 kHz".
///  - **Hz vs kHz.** `quality_state::detail` normalizes either unit (the
///    `>= 1000` guard) exactly like the track rows do, so the stamp and the
///    row can never disagree. The forked `format!` printed a raw Hz value as
///    "44100 kHz".
///  - **The empty-detail gate.** The reference blanks the line only when the
///    TIER is unknown or BOTH params are missing; the fork blanked it whenever
///    either one was missing, so a track with a known rate and no depth lost
///    its whole detail line instead of taking the formatter's depth default.
async fn quality_badge(track: &QueueTrack) -> (String, String) {
    // Server-backed rows already retain their codec/container in the source
    // cache, but QueueTrack predates that vocabulary. Ask the owning source
    // once on the track edge instead of guessing from decoded PCM: an MP3
    // decodes to 16-bit samples too, and calling that "CD" is a lie.
    let source_hint = match qbz_source::SourceBadge::from_word(track.source.as_deref()) {
        qbz_source::SourceBadge::Plex
        | qbz_source::SourceBadge::Jellyfin
        | qbz_source::SourceBadge::Subsonic => {
            let raw = qbz_source::RawRef::from_queue_track(track);
            match qbz_source::registry().claim(&raw) {
                Ok(item) => qbz_source::registry()
                    .meta(&item)
                    .await
                    .ok()
                    .map(|meta| meta.quality),
                Err(_) => None,
            }
        }
        _ => None,
    };
    quality_badge_from(track, source_hint)
}

fn quality_badge_from(
    track: &QueueTrack,
    source_hint: Option<qbz_source::QualityHint>,
) -> (String, String) {
    let bit_depth = track
        .bit_depth
        .or_else(|| source_hint.and_then(|hint| hint.bit_depth));
    let sample_rate = track
        .sample_rate
        .or_else(|| source_hint.and_then(|hint| hint.sample_rate_khz));
    let is_mp3 = source_hint.is_some_and(|hint| hint.format.eq_ignore_ascii_case("mp3"));
    if is_mp3 {
        let label = sample_rate
            .filter(|rate| *rate > 0.0)
            .map(|rate| {
                let khz = if rate >= 1000.0 { rate / 1000.0 } else { rate };
                format!("{} kHz · MP3", crate::home_qt::format_rate(khz))
            })
            .unwrap_or_else(|| "MP3".to_string());
        return ("mp3".to_string(), label);
    }

    let tier = match bit_depth {
        // 1-bit is DSD, and it wears its own mark: the Hi-Res logo on a DSD
        // master described a 24/352.8 stream Qobuz never serves.
        Some(1) => "dsd",
        Some(d) if d >= 24 => "hires",
        Some(_) => "cd",
        None if sample_rate.is_some_and(|rate| rate > 0.0) => "cd",
        None if track.hires => "hires",
        None => "",
    }
    .to_string();
    let label = if tier.is_empty() || (bit_depth.is_none() && sample_rate.is_none()) {
        String::new()
    } else if bit_depth == Some(1) {
        crate::quality_state::dsd_multiple_label(sample_rate)
    } else {
        crate::quality_state::detail(bit_depth, sample_rate)
    };
    (tier, label)
}

fn purchase_quality_badge(
    format_id: u32,
) -> Option<(&'static str, &'static str, Option<u32>, Option<f64>)> {
    match format_id {
        56 => Some(("dsd", "DSD128", Some(1), Some(5_644_800.0))),
        55 => Some(("dsd", "DSD64", Some(1), Some(2_822_400.0))),
        27 => Some(("hires", "24-bit / 192 kHz", Some(24), Some(192.0))),
        7 => Some(("hires", "24-bit / 96 kHz", Some(24), Some(96.0))),
        6 => Some(("cd", "16-bit / 44.1 kHz", Some(16), Some(44.1))),
        5 => Some(("mp3", "320 kbps · MP3", None, None)),
        _ => None,
    }
}

/// Publish the queue panel document (queue_qt.rs — the full QueueState
/// port: sections, history, pagination, search).
pub async fn publish_queue(runtime: &Arc<AppRuntime<LoggingAdapter>>) {
    crate::queue_qt::publish(runtime).await;
}

// ---------------------------------------------------------------------------
// 1 Hz state pump (playback.rs `start_poll_loop`, minimized)
// ---------------------------------------------------------------------------

static POLL_STARTED: AtomicBool = AtomicBool::new(false);
/// Bumped when the local Shuffle control changes the queue timeline. The poll
/// owns the gapless task/one-shot guard, so this is its cheap invalidation
/// channel for an in-flight successor that was derived from the old order.
static QUEUE_ORDER_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Wall-clock now in ms — the peer-position extrapolation clock (the Slint
/// `now_ms`, playback.rs:5162).
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// Start the 1 Hz pump on shell entry (idempotent). Publishes position /
/// playing / cache into the NowPlayingModel, detects track changes (meta +
/// queue refresh), and advances the queue at end-of-track through the core.
pub fn start_poll_loop(runtime: Arc<AppRuntime<LoggingAdapter>>) {
    if POLL_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    crate::spawn(async move {
        let mut last_track_id: u64 = 0;
        // The id of a track that just ENDED, while the poll has advanced past
        // it and the successor's stream has not started yet. The player keeps
        // reporting the ended id through that window (CMAF session + init
        // segment, or a DSD→PCM conversion: 1-2 s), and with `last_track_id`
        // reset by the recovery, the edge block below read it as a NEW track:
        // `sync_current_to_id` rewound the queue cursor to the previous row,
        // now-playing/MPRIS/notifications flip-flopped old → new → old (three
        // notifications per track change, the middle one a ghost), and a
        // gapless stage that sampled `peek_upcoming` in that window was handed
        // the track already playing (smoke 2026-09-02: Hangar 18 twice).
        let mut stale_after_advance: u64 = 0;
        // Exact owner generation that produced the last local snapshot. Keep
        // this separate from `last_track_id`: an owner restored after guest
        // playback may legitimately resume the same numeric track id, and
        // still owes owner-only edges/integrations from its fresh snapshot.
        let mut last_owner_token: Option<OwnerActionToken> = None;
        let mut was_playing = false;
        // Idle L1 trim: the in-memory audio cache (400 MB budget) keeps every
        // track played until another evicts it, so an app left idle after a
        // listening session sat ~200-400 MB above its baseline (measured
        // 2026-09-02). After IDLE_L1_TRIM_SECS with nothing audible — local,
        // remote or cast — the L1 entries move to the L2 disk cache, which
        // still serves a replay without a download. Trimmed once per idle
        // stretch; any audible tick re-arms it.
        let mut last_audible = std::time::Instant::now();
        let mut l1_trimmed = false;
        let mut seen_position: u64 = 0;
        // Divider for the ~5s position flush (see the save_position call).
        let mut save_pos_tick: u64 = 0;
        // Track id we have already fired a gapless prefetch for, so this poll
        // does not re-request it on every tick while the download is in flight
        // (playback.rs:5049-5051).
        let mut gapless_requested_for: u64 = 0;
        let mut seen_queue_order_generation = QUEUE_ORDER_GENERATION.load(Ordering::SeqCst);
        // Own the frontend's gapless fetch so an engine-empty recovery can
        // abort a segment body that is still monopolizing the link. The
        // player's incremental successor feeder has its own cancellation slot;
        // this handle covers the outer tier-walk task.
        let mut gapless_fetch_task: Option<tokio::task::JoinHandle<()>> = None;
        // Durable player edges consumed by this poller. Both begin at zero;
        // the player generations are monotonic for the process lifetime.
        let mut seen_engine_empty_generation: u64 = 0;
        let mut seen_source_failure_generation: u64 = 0;
        // Immediate full-cache warming has a distinct edge because it follows
        // both the local engine and the cast cursor. Unlike the gapless guard,
        // it names the NEW current track whose two successors are being warmed.
        let mut prefetch_track_edge = PrefetchTrackEdge::default();
        let mut seen_owner_playback_task_generation = owner_playback_task_generation();

        // QConnect renderer-report throttle (contract §6.2): the official
        // client reports RndrSrvrStateUpdated ~every 2s while playing PLUS
        // immediately on a transition (track / play-state change). The Slint
        // ticks at 450ms and reports every 4 ticks ≈ 1.8s
        // (playback.rs:5053-5059); this loop ticks at 1 Hz, so the wall-clock
        // equivalent is every 2 ticks — tick-count throttles do NOT translate
        // 1:1, translate by wall time.
        let mut last_reported_track_id: u64 = 0;
        let mut last_reported_playing = false;
        let mut last_reported_buffer_state = qbz_player::player::PlaybackBufferState::Idle;
        let mut report_tick: u64 = 0;
        const QCONNECT_REPORT_EVERY_N_TICKS: u64 = 2;

        // QConnect CONTROLLER mode (contract §6.1): the peer's last-seen
        // current track id. When the peer advances a track on its own, this
        // edge-detects the change so the bar/queue meta refresh from the core
        // cursor (which the sink already aligned to the peer's track). Reset
        // to 0 when the peer branch is not taken so re-entering controller
        // mode refreshes meta (playback.rs:5061-5066).
        let mut last_peer_track_id: u64 = 0;
        // Dirty-guard for the per-tick peer UI pushes (playback.rs:5194-5204):
        // (track_id, position_ms, duration_secs, playing, volume f32 bits,
        // muted, shuffle, repeat_mode). Re-pushing identical values every second
        // dirties bindings and repaints even when fully idle. `track_id` is
        // load-bearing: it guarantees the push after a peer track change (the
        // meta refresh just reset the bar's position to 0). Reset to None
        // whenever another owner (local/cast poll) may have written the bar.
        let mut last_remote_ui_push: Option<(u64, u64, u64, bool, u32, bool, bool, i32)> = None;
        // Physical ALSA knob events update the Player's shared volume atomics.
        // Mirror only hardware-volume edges onto QML; ordinary local slider
        // writes already publish immediately at their command boundary.
        let mut last_hardware_volume: Option<(bool, u32)> = None;

        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(1));
        loop {
            ticker.tick().await;

            let task_generation = owner_playback_task_generation();
            if task_generation != seen_owner_playback_task_generation {
                // A transition fence cancelled every admitted background
                // download. Rearm both one-shot edges so a failed candidate can
                // resume owner warming; a committed guest remains gated by its
                // delegated observation and cannot start replacement work.
                seen_owner_playback_task_generation = task_generation;
                gapless_requested_for = 0;
                prefetch_track_edge.observe(0);
            }

            let queue_order_generation = QUEUE_ORDER_GENERATION.load(Ordering::SeqCst);
            if queue_order_generation != seen_queue_order_generation {
                seen_queue_order_generation = queue_order_generation;
                gapless_requested_for = 0;
                if let Some(task) = gapless_fetch_task.take() {
                    if !task.is_finished() {
                        log::info!(
                            "[qbz-qt] [GAPLESS] aborting successor fetch after queue order changed"
                        );
                        task.abort();
                    }
                }
            }

            if gapless_fetch_task
                .as_ref()
                .is_some_and(|task| task.is_finished())
            {
                if let Some(task) = gapless_fetch_task.take() {
                    if let Err(error) = task.await {
                        if !error.is_cancelled() {
                            log::warn!("[qbz-qt] gapless fetch task failed: {error}");
                        }
                    }
                }
            }

            // --- Stream-failure toast (PARITY-DEBT #7) -------------------
            // Surface audio-stream failures as a toast, 1:1 with
            // `playback.rs:5093-5115` (#508/#534/#500). The player records a
            // user-readable message when a stream fails to open; this port
            // drained the slot and only logged it, so an ALSA Direct /
            // exclusive-mode failure left the bar frozen at 0:00 with no
            // explanation — the support issue the Slint fix was written for.
            // take_stream_error_message() drains exactly once per recorded
            // error, so each failed attempt toasts once.
            {
                // The error slot is itself a playback snapshot. Admit before
                // draining it so a guest error cannot be applied to an owner
                // row after a guest -> owner promotion.
                let (error_owner_snapshot, stream_error) =
                    match capture_owner_scoped_snapshot(observe_owner_action, || {
                        runtime.core().player().state.take_stream_error_message()
                    }) {
                        OwnerScopedSnapshot::Captured {
                            owner_action,
                            snapshot,
                        } => (owner_action, snapshot),
                        // Do not drain an owner error or perturb playback edge
                        // bookkeeping merely because a candidate is still inside
                        // its fallible, pre-commit fence.
                        OwnerScopedSnapshot::Fenced => continue,
                    };
                if let Some(msg) = stream_error {
                    log::error!("[qbz-qt] audio output error: {msg}");
                    let sandboxed = !matches!(
                        qbz_audio::health::detect_sandbox(),
                        qbz_audio::health::Sandbox::None
                    );
                    crate::toast_qt::error(stream_error_text(&msg, sandboxed));
                    // Listen log: only the owner observation may close its row.
                    // Guest playback is closed explicitly as `handoff` at the
                    // authority boundary and is never folded into this store.
                    if let Some(owner_snapshot) = error_owner_snapshot.as_ref() {
                        crate::listen_log_qt::on_error(owner_snapshot).await;
                    }
                }
            }

            // --- QConnect CONTROLLER mode: peer-state reflection ----------
            // (contract §6.1 — Slint playback.rs:5117-5244, inserted BEFORE
            // the cast branch exactly like the reference.) When QBZ is
            // CONTROLLING a peer renderer, the event sink stops the LOCAL
            // player, so `get_playback_event()` reports track_id == 0 / not
            // playing and the seek bar would freeze. While a peer owns
            // playback, drive the bar from the peer's renderer snapshot
            // instead: title / artist / art come from the materialized local
            // core queue (the sink aligns the core cursor to the peer's
            // track), only position / playing / duration come from the peer.
            // Returns None in every NON-controller situation (disconnected,
            // renderer mode where active == local, no active renderer), so
            // the local path below runs byte-unchanged.
            //
            // UNITS (contract §6): `RemoteNowPlaying.position_ms` /
            // `updated_at_ms` are MILLISECONDS; `now_playing::set_position`
            // takes SECONDS. Convert at the publish boundary.
            if let Some(remote) = match crate::qconnect_qt::service() {
                Some(svc) => svc.remote_now_playing().await,
                None => None,
            } {
                // Queue metadata below belongs to this controller-side owner
                // observation. A concurrent delegation invalidates it before
                // any queue read or integration task can be admitted.
                let Some(remote_owner_snapshot) = begin_owner_action() else {
                    last_peer_track_id = 0;
                    last_remote_ui_push = None;
                    continue;
                };
                // The peer changed track on its own → refresh the bar/queue
                // meta from the core cursor (the event sink already aligned
                // it to the peer's track via sync_active_renderer_projection).
                // This resets position to 0, which is correct on a track
                // change; the per-tick position push below immediately
                // re-applies the peer's real position. Done BEFORE the
                // per-tick push so peer values win.
                if remote.track_id != last_peer_track_id {
                    refresh_now_playing(&runtime).await;
                    publish_queue(&runtime).await;
                    // Discord follows the peer (§11.2): the peer track edge
                    // pushes Discord but NEVER scrobbles — 1:1 with the
                    // Slint, where this edge reaches the UNGUARDED push
                    // inside refresh_now_playing_meta (playback.rs:5137-5141
                    // -> :2236-2239). The scrobble half of the LOCAL edge is
                    // peer-guarded inside `on_track_change_edge` instead.
                    crate::integrations_qt::discord_track_change_edge(
                        &runtime,
                        remote_owner_snapshot.token(),
                        remote.track_id,
                    );
                    last_peer_track_id = remote.track_id;
                }
                // Lyrics follow the peer (Q7, §11.1): publish the RAW
                // renderer anchor on EVERY peer-branch tick — the lyrics
                // sync invokables extrapolate between poll ticks exactly
                // like the position extrapolation below
                // (playback.rs:5142-5149). Outside the track edge AND the
                // dirty guard on purpose: re-anchoring a peer pause promptly
                // beats a skipped UI push.
                crate::lyrics_qt::publish_remote_anchor(
                    remote.position_ms,
                    remote.updated_at_ms,
                    remote.playing,
                );
                // Duration from the core queue's current track (aligned to the
                // peer's track by the sink). Zero when unknown — the clamp is
                // skipped then; without it the seekbar is wrong.
                let duration_secs = runtime
                    .core()
                    .current_track()
                    .await
                    .map(|track| track.duration_secs)
                    .unwrap_or(0);
                let duration_ms = duration_secs.saturating_mul(1000);
                // Extrapolate position while playing; clamp to the track length.
                let mut position_ms = remote.position_ms;
                if remote.playing && remote.updated_at_ms > 0 {
                    position_ms =
                        position_ms.saturating_add(now_ms().saturating_sub(remote.updated_at_ms));
                }
                if duration_ms > 0 && position_ms > duration_ms {
                    position_ms = duration_ms;
                }
                // ms → s at the publish boundary (see UNITS above).
                let position_secs = position_ms / 1000;
                let playing = remote.playing;
                // The event sink unlocks controls once the peer reports volume.
                let remote_volume = crate::qconnect_qt::peer_volume_fraction(remote.volume);
                // Reflect the PEER's shuffle/repeat state on the bar buttons.
                // Pure UI reflection of the cloud's reported state — no local
                // order is generated (WS-authoritative for shuffle order).
                let shuffle_on = remote.shuffle_mode;
                let repeat_mode = remote.repeat_mode;
                // Skip the UI hop when nothing the push depends on changed
                // since the last push (see the dirty-guard comment at the
                // trackers). While playing, `position_ms` extrapolates every
                // tick, so pushes proceed; paused, the bar stops repainting.
                let remote_snapshot = (
                    remote.track_id,
                    position_ms,
                    duration_secs,
                    playing,
                    remote_volume.to_bits(),
                    remote.muted,
                    shuffle_on,
                    repeat_mode,
                );
                if last_remote_ui_push != Some(remote_snapshot) {
                    last_remote_ui_push = Some(remote_snapshot);
                    // Keep the visualizer producer in step with the same flag
                    // the drain gate reads (a paused PEER parks it too;
                    // playback.rs:5205-5209).
                    crate::viz_qt::set_paused(!playing);
                    // The peer branch `continue`s past the local body that
                    // re-sets the seek lock every tick — force it open here
                    // (playback.rs:5219) or a mid-stream takeover would latch
                    // the last buffer fraction and refuse routed seeks.
                    crate::now_playing::set_seekable_max(1.0);
                    // `has_audio = true` clears the fetch spinner: a peer owns
                    // audio, so there is no local fetch wait and the bar's
                    // spinner must never linger here (the Slint force-clears
                    // PENDING_PLAY_ID on the peer edge, playback.rs:5228-5233
                    // — Qt has no PENDING_PLAY_ID/clear_loading; this push is
                    // guaranteed on the edge because track_id is in the
                    // dirty-guard tuple). Cache overlay 0.0 = "not streaming",
                    // same value the cast poll publishes (cast_qt.rs:1000).
                    crate::now_playing::set_position(
                        position_secs as i32,
                        duration_secs as i32,
                        playing,
                        0.0,
                        true,
                    );
                    crate::now_playing::set_volume(remote_volume);
                    crate::now_playing::set_muted(remote.muted);
                    crate::now_playing::set_shuffle(shuffle_on);
                    crate::now_playing::set_repeat_mode(repeat_mode);
                }
                // Reset the LOCAL edge trackers so when control returns to QBZ
                // the end-of-track / gapless / transition logic re-detects
                // from a clean slate (the local player was stopped while
                // peer-active).
                last_track_id = 0;
                was_playing = false;
                seen_position = 0;
                // A peer renderer owns its own playback/cache. Rearm only so a
                // later hand-back to local/cast can warm from that real edge.
                prefetch_track_edge.observe(0);
                continue;
            }

            // CAST: the cast service runs its own 1 s poll and the local player is
            // stopped, so this path would push position 0 / not-playing every
            // second and fight it.
            if crate::cast_qt::is_casting().await {
                let Some(cast_owner_snapshot) = begin_owner_action() else {
                    last_track_id = 0;
                    was_playing = false;
                    seen_position = 0;
                    prefetch_track_edge.observe(0);
                    continue;
                };
                // The local engine is stopped while casting, so its PlaybackEvent can
                // never surface a track edge. The cast service advances the shared
                // core cursor; observe that cursor here and warm once per new current
                // track at the renderer-effective tier.
                let cast_track_id = runtime
                    .core()
                    .current_track()
                    .await
                    .map(|track| track.id)
                    .unwrap_or(0);
                if prefetch_track_edge.observe(cast_track_id) {
                    kick_prefetch(&runtime, &cast_owner_snapshot).await;
                }
                last_track_id = 0;
                was_playing = false;
                seen_position = 0;
                // Same invariant for the PEER edge trackers (playback.rs:5268-5269):
                // the remote branch above did not run, so without this reset a
                // re-taken peer whose snapshot happens to match the last controller
                // push would be skipped, leaving the cast renderer's values on the
                // bar (mirrors the fall-through reset below).
                last_peer_track_id = 0;
                last_remote_ui_push = None;
                // A cast target owns playback and there is no local download to
                // outrun: seek is unlocked while casting, the way the Slint cast
                // publish forces it (`cast_service.rs:1118`). Without this the last
                // local stream's fraction would stay latched on the bar.
                crate::now_playing::set_seekable_max(1.0);
                continue;
            }
            // Not in controller mode (no peer / returned to local): reset the
            // peer-track edge var so re-entering the peer state refreshes meta
            // (playback.rs:5274-5277).
            if last_remote_ui_push.is_some() {
                if let Some(_owner_action) = begin_owner_action() {
                    // The peer's mute was UI-only; returning to local restores
                    // the owner's retained toggle without changing its volume.
                    crate::now_playing::set_volume(runtime.core().get_playback_state().volume);
                    crate::now_playing::set_muted(MUTED.load(Ordering::Relaxed));
                }
            }
            last_peer_track_id = 0;
            last_remote_ui_push = None;
            // §11.1: deliberately NO lyrics anchor clear here (the Slint clears
            // per-tick at playback.rs:5279) — the Qt anchor clears from the single
            // `now_playing::set_remote(false)` choke point, which unlike this
            // fallthrough also runs while the cast branch owns the loop. See the
            // note in `now_playing::set_remote`.
            // Freeze owner authority before reading any local playback or queue
            // snapshot. `None` is a valid delegated observation: UI/QConnect
            // reporting remain live, but owner-only consumers must use this
            // exact result and may not obtain a fresh permit later in the tick.
            let (owner_snapshot, event) =
                match capture_owner_scoped_snapshot(observe_owner_action, || {
                    runtime.core().player().get_playback_event()
                }) {
                    OwnerScopedSnapshot::Captured {
                        owner_action,
                        snapshot,
                    } => (owner_action, snapshot),
                    // Preserve last_track_id / last_owner_token and every owner
                    // integration across a candidate that may still fail. Only an
                    // actually installed guest is an authority edge.
                    OwnerScopedSnapshot::Fenced => continue,
                };
            let track_id = event.track_id;
            let position = event.position;
            let duration = event.duration;
            let is_playing = event.is_playing;
            if is_playing || crate::now_playing::remote_or_cast_active() {
                last_audible = std::time::Instant::now();
                l1_trimmed = false;
            } else if !l1_trimmed && last_audible.elapsed().as_secs() >= IDLE_L1_TRIM_SECS {
                l1_trimmed = true;
                log::info!(
                    "[qbz-qt] idle for {IDLE_L1_TRIM_SECS}s — evicting the L1 audio cache to disk"
                );
                runtime.core().player().evict_l1_audio_cache();
            }
            let cache = event.buffer_progress.unwrap_or(0.0);
            let hardware_volume = (event.hardware_volume_active, event.volume.to_bits());
            if last_hardware_volume != Some(hardware_volume) {
                if event.hardware_volume_active
                    || last_hardware_volume.is_some_and(|(active, _)| active)
                {
                    crate::now_playing::set_volume(event.volume);
                }
                last_hardware_volume = Some(hardware_volume);
            }
            // Seek lock (PARITY-DEBT #15, playback.rs:5304): while streaming
            // (`buffer_progress` is Some) the user can only seek up to what
            // has downloaded; a fully-available track (None) seeks freely.
            // `cache` above is the DECORATIVE overlay of the same fill — this
            // is the value the seek bars must enforce.
            let seekable_max = event
                .buffer_progress
                .map(|p| p.clamp(0.0, 1.0))
                .unwrap_or(1.0);

            // This observation is cheap and happens each tick; the work does
            // not. Only a NEW non-zero engine track — including a gapless
            // handoff — enters `kick_prefetch`. A stop rearms replaying the
            // same id later.
            let immediate_prefetch_edge = if owner_snapshot.is_some() {
                prefetch_track_edge.observe(track_id)
            } else {
                // Do not let a guest id consume the owner's one-shot edge. A
                // restored owner whose track id happens to match must warm its
                // own queue from a fresh owner snapshot.
                prefetch_track_edge.observe(0);
                false
            };

            // --- Track-change edge (new current track surfaced) ----------
            let observed_owner_token = owner_snapshot.as_ref().map(OwnerActionLease::token);
            // Compare the COMPLETE owner identity, not only None -> Some. A
            // short owner -> guest -> owner cycle can fit between two poll
            // ticks; its restored owner token is new even when neither the
            // numeric track id nor the sampled Option-ness changed. The old
            // delayed integrations were invalidated by that authority cycle,
            // so this fresh snapshot must re-arm their edge work.
            let owner_changed = owner_generation_changed(observed_owner_token, last_owner_token);
            // The ended track, still reported while its successor is loading,
            // is NOT an edge (see `stale_after_advance`). Only while nothing
            // plays: the moment audio runs, the reported id is the truth again
            // — including a deliberate replay of that same track.
            if stale_after_advance != 0 {
                if track_id == stale_after_advance && !is_playing {
                    seen_position = position;
                    was_playing = is_playing;
                    last_owner_token = observed_owner_token;
                    continue;
                }
                stale_after_advance = 0;
            }
            if track_id != 0 && (track_id != last_track_id || owner_changed) {
                if owner_snapshot.is_some() {
                    // A cold OWNER start that reached audio is no longer
                    // pending. Guest playback must not consume this latch.
                    PENDING_PLAY_ID.store(0, Ordering::Relaxed);
                }
                // Reconcile the queue pointer (a real advance moves it; a
                // manual play already did). A delegated renderer installs its
                // cursor through the stamped QConnect engine, so this owner
                // poller must never realign it.
                if owner_snapshot.is_some() {
                    let _ = runtime.core().sync_gapless_successor(track_id).await;
                }
                // UI reflection is authority-neutral: the local renderer must
                // still paint and report guest playback.
                refresh_now_playing(&runtime).await;
                publish_queue(&runtime).await;
                if let Some(owner_snapshot) = owner_snapshot.as_ref() {
                    // Integrations: scrobble now-playing + arm the delayed
                    // scrobble, and refresh the Discord presence. This is the
                    // DE-DUPED track edge on purpose — firing it from
                    // refresh_now_playing would re-arm the timer on every republish.
                    crate::integrations_qt::on_track_change_edge(
                        &runtime,
                        owner_snapshot.token(),
                        track_id,
                    );
                    // Local play history — Recently Played and Most Played read the
                    // same file the other frontends write; without this the Qt build
                    // shows their history and never adds to it. Same de-duped edge
                    // as the scrobblers, never from refresh_now_playing.
                    //
                    // EPHEMERAL (owner rule): an ephemeral folder "solo se reproduce
                    // y ya" and "no deja rastros en las bibliotecas" — it may be
                    // added to the queue and to nothing else, so a play of it must
                    // NOT land in Recently Played / Most Played. The player carries
                    // the synthetic id verbatim (`local_ephemeral::play_file` hands
                    // `row_id` to the engine), so the poll's own `track_id` is the
                    // cheapest place to decide, before the queue-state fetch.
                    // `record_queue_track` re-checks the same predicate, so the rule
                    // holds even if another call site appears.
                    if !crate::local_ephemeral::is_ephemeral_id(track_id as i64) {
                        if let Some(track) = runtime.core().get_queue_state().await.current_track {
                            crate::recently_qt::record_queue_track(&track);
                            crate::home_qt::note_recent_store_changed();
                            // Listen log: open the row for this track (and close
                            // the previous one as a skip if it is still open).
                            // Runs BESIDE the old stores, never instead of them.
                            crate::listen_log_qt::on_track_edge(owner_snapshot, &track, &event)
                                .await;
                        }
                    }
                    // Persist the session (queue + current track + position) so a
                    // restart can restore it. Hooked on the DE-DUPED track edge and
                    // not at each play call site: `play_queue_track` alone has five
                    // early returns (cast, peer, local, plex, Qobuz) and the album /
                    // single / shuffle entries add more, so a per-call-site hook is
                    // a list that silently goes stale. This edge sees them all.
                    // No-op unless `persist_session` is on.
                    qbz_app::session_persist::capture_and_save(&runtime).await;
                }
                last_track_id = track_id;
                crate::ab_loop_qt::on_track_changed(track_id);
                // The engine may have reached this track through a GAPLESS
                // hand-off, in which case the arming guard still names the
                // track that just ended. Clear it so the NEW current track can
                // arm its own prefetch (playback.rs:5353).
                if let Some(task) = gapless_fetch_task.take() {
                    if !task.is_finished() {
                        log::info!(
                            "[qbz-qt] [GAPLESS] aborting stale fetch after track changed to {track_id}"
                        );
                        task.abort();
                    }
                }
                gapless_requested_for = 0;
            }

            // Separate from the UI edge above on purpose: a 0-id stop rearms
            // this tracker, so replaying the same queue id warms again even if
            // another local edge tracker has not yet been cleared by its
            // end-of-track arm.
            if immediate_prefetch_edge {
                if let Some(owner_snapshot) = owner_snapshot.as_ref() {
                    kick_prefetch(&runtime, owner_snapshot).await;
                }
            }

            if track_id != 0 {
                log::debug!("[qbz-qt] poll: id={track_id} pos={position}/{duration} playing={is_playing} cache={cache:.2}");
            }
            // Listen log accumulator: credits audible seconds only (playing,
            // monotonic, <= 5 s per tick); a run of empty ticks is the stop.
            if let Some(owner_snapshot) = owner_snapshot.as_ref() {
                crate::listen_log_qt::on_tick(owner_snapshot, track_id, position, is_playing).await;
            }

            // --- Position / state push ------------------------------------
            // Seek lock first, so the bar never renders a position push whose
            // limit still belongs to the previous tick (the setter dedupes,
            // so a fully-available track costs no extra hop).
            crate::now_playing::set_seekable_max(seekable_max);
            crate::now_playing::publish_seek_waveform(track_id);
            crate::now_playing::set_position(
                position as i32,
                duration as i32,
                is_playing,
                cache,
                track_id != 0,
            );
            // The desktop's copy of the same number. Read-on-demand, so it has
            // to be kept fresh rather than signalled — see push_position.
            crate::media_controls_qt::push_position(position);

            // DELIVERED stream params -> the downgrade arrow + tooltip cause.
            // Deduped inside, so a steady stream costs no Qt-thread hop.
            // The mode the STREAM negotiated, not the one the settings asked
            // for. `output_labels.rs` already derives the LED from settings;
            // this is the only value that can contradict it, which is exactly
            // what makes it worth publishing.
            let bit_perfect = match event.bit_perfect_mode {
                Some(qbz_audio::BitPerfectMode::DirectHardware) => "direct",
                Some(qbz_audio::BitPerfectMode::PluginFallback) => "plugin",
                Some(qbz_audio::BitPerfectMode::Disabled) => "disabled",
                None => "unknown",
            };
            crate::now_playing::set_effective_stream(
                event.sample_rate.unwrap_or(0),
                event.bit_depth.unwrap_or(0),
                bit_perfect,
            );

            // --- Gapless prefetch trigger (playback.rs:5362-5476) ---------
            // The audio engine ASKS for the next track ~10s before the end of
            // the current one ("Gapless: approaching end of track", player/
            // mod.rs:3927) by raising `gapless_ready`. Nothing answered it in
            // this port, so every advance went through the end-of-track edge
            // below instead: fetch AFTER silence, ~1s of nothing plus the
            // initial-buffer wait. The engine, the setting and the CMAF path
            // were all working — this seam simply was not wired.
            //
            // Answering means handing the engine the next track's BYTES while
            // the current one still plays, via `Player::play_next`, which
            // appends to the same rodio sink and is what makes the transition
            // sample-accurate.
            // The stop-after guard is last and short-circuited. It is LIVE now
            // (the queue row menu can place the marker): without it the engine
            // would hand the next track's bytes over seamlessly and the marker
            // would never fire, which is exactly what playback.rs:5372-5377
            // warns about.
            if owner_snapshot.is_some()
                && event.gapless_ready
                && event.gapless_next_track_id == 0
                && track_id != 0
                && gapless_requested_for != track_id
                && runtime.core().get_stop_after().await != Some(track_id)
            {
                if let Some(owner_snapshot) = owner_snapshot.as_ref() {
                    let owner_token = owner_snapshot.token();
                    let upcoming = runtime.core().peek_upcoming(1).await;
                    if let Some(next) = upcoming.into_iter().next() {
                        // Never queue the current track as its own successor —
                        // by the poll's id AND by the player's own: the two
                        // disagree exactly when the queue cursor is off by one
                        // (the stale-edge rewind above), and a successor equal
                        // to what is playing plays the track twice.
                        let playing_now = runtime.core().player().state.current_track_id();
                        if next.id == playing_now {
                            log::warn!(
                                "[qbz-qt] [GAPLESS] upcoming {} is the track already playing; cursor off by one, successor skipped",
                                next.id
                            );
                        } else if next.id != track_id
                            && gapless_source(&next) == GaplessSource::Download
                        {
                            // Local Library downloads carry a catalog play ID
                            // and a local row hint, but their path is an encrypted
                            // bundle. Use the same offline resolver as normal
                            // playback, before the generic local-file branch.
                            gapless_requested_for = track_id;
                            let runtime = runtime.clone();
                            let next_id = next.id;
                            let task = spawn_owner_playback_task(async move {
                                let Some(_owner_action) =
                                    begin_owner_action_exact(owner_token).await
                                else {
                                    return;
                                };
                                let Some(offline) = crate::offline_qt::get().await else {
                                    return;
                                };
                                let sink = crate::offline_cache_qt::row_sink();
                                let Some(bytes) = qbz_core::offline_resolve::resolve_offline_bytes(
                                    next_id,
                                    &offline,
                                    Some(&sink),
                                )
                                .await
                                else {
                                    return;
                                };
                                if !gapless_edge_is_current(&runtime, track_id, next_id).await {
                                    return;
                                }
                                if let Err(error) =
                                    runtime.core().player().play_next(bytes, next_id)
                                {
                                    log::warn!(
                                        "[qbz-qt] [GAPLESS] offline successor {next_id}: {error}"
                                    );
                                }
                            });
                            if let Some(stale) = gapless_fetch_task.replace(task) {
                                stale.abort();
                            }
                        } else if next.id != track_id
                            && gapless_source(&next) == GaplessSource::Catalog
                        {
                            gapless_requested_for = track_id;
                            let runtime = runtime.clone();
                            let next_id = next.id;
                            let next_album_id = next.album_id.clone();
                            let task = spawn_owner_playback_task(async move {
                                let Some(_owner_action) =
                                    begin_owner_action_exact(owner_token).await
                                else {
                                    return;
                                };
                                // A purchased copy of the successor on disk is
                                // queued from the file — DSD through the
                                // player's DSD append, PCM as bytes — before
                                // the streaming/tier-walk arms get a look, so
                                // an album bought as DSD stays DSD across the
                                // gapless edge instead of flipping to the
                                // stream on track two.
                                if let Some(resolution) =
                                    crate::purchase_playback_qt::resolve_preferred_copies(
                                        next_id,
                                        next_album_id.as_deref(),
                                    )
                                    .await
                                {
                                    for copy in &resolution.copies {
                                        let ticket = match prepare_purchase_gapless_ticket(
                                            &runtime, copy, next_id,
                                        )
                                        .await
                                        {
                                            Ok(ticket) => ticket,
                                            Err(error) => {
                                                log::warn!(
                                                    "[qbz-qt] [GAPLESS] purchase copy {} of {next_id} could not be prepared: {error}; trying the next exact copy",
                                                    copy.copy_id
                                                );
                                                continue;
                                            }
                                        };
                                        if !gapless_edge_is_current(&runtime, track_id, next_id)
                                            .await
                                        {
                                            log::info!(
                                                "[qbz-qt] [GAPLESS] discarded stale purchase resolution for track {next_id}"
                                            );
                                            return;
                                        }
                                        match crate::audible_qt::queue_gapless_ticket(
                                            &runtime,
                                            track_id,
                                            next_id,
                                            ticket,
                                        )
                                        .await
                                        {
                                            Ok(true) => {
                                                crate::purchase_playback_qt::remember_materialized_copy(
                                                    next_id,
                                                    &resolution,
                                                    copy,
                                                );
                                                log::info!(
                                                    "[qbz-qt] [GAPLESS] queued track {next_id} from purchase copy {} (format {})",
                                                    copy.copy_id,
                                                    copy.format_id
                                                );
                                                return;
                                            }
                                            Ok(false) => {
                                                log::info!(
                                                    "[qbz-qt] [GAPLESS] purchase copy of {next_id} not queued (predecessor moved or a successor is already set)"
                                                );
                                                return;
                                            }
                                            Err(error) => log::warn!(
                                                "[qbz-qt] [GAPLESS] purchase copy {} of {next_id} failed: {error}; trying the next exact copy",
                                                copy.copy_id
                                            ),
                                        }
                                    }
                                    log::warn!(
                                        "[qbz-qt] [GAPLESS] all complete purchase copies of {next_id} (format {}) failed; falling back to Qobuz",
                                        resolution.format_id
                                    );
                                }
                                crate::purchase_playback_qt::clear_materialized_track(next_id);
                                let quality = local_playback_quality().0;
                                let streaming_only =
                                    crate::settings_qt::audio_settings().streaming_only;
                                let player_cached =
                                    runtime.core().player().is_track_cached(next_id);
                                let offline_cached = crate::offline_qt::is_cached_id(next_id);

                                if !offline_cached {
                                    // L1/L2 and cold catalog successors all use the
                                    // bounded streaming/file path. Offline downloads
                                    // retain the existing offline-aware resolver below.
                                    let throttle_cap = qbz_audio::network_throttle::state()
                                        .current_prefetch_cap(
                                            qbz_audio::network_throttle::playback_mbps_for_quality(prefetch_quality_tag(quality)), 1);
                                    if !should_stream_gapless_successor(streaming_only, player_cached, offline_cached, throttle_cap) {
                                        log::info!("[qbz-qt] [GAPLESS] successor {next_id} skipped: network throttle cap 0");
                                        return;
                                    }
                                    match runtime.core().queue_gapless_streaming(next_id, quality).await {
                                        Ok(()) => log::info!("[qbz-qt] [GAPLESS] queued streaming track {next_id}"),
                                        Err(error) => log::warn!("[qbz-qt] [GAPLESS] streaming setup for {next_id} failed: {error}"),
                                    }
                                    return;
                                }

                                // Shared tier-walk: L1/L2 (player cache) -> OFFLINE
                                // -> network, then hand the bytes to play_next. The
                                // offline handle and the row sink are the same two
                                // `play_resolved_offline_aware` supplies; passing
                                // `None, None` here (under a comment claiming this
                                // port opens no offline index — the funnel above
                                // does) made a DOWNLOADED next track re-download for
                                // gapless, and fail outright with no network.
                                let off = crate::offline_qt::get().await;
                                let sink =
                                    off.as_ref().map(|_| crate::offline_cache_qt::row_sink());
                                if let Some(data) = runtime
                                    .core()
                                    .fetch_for_gapless_resolved(
                                        next_id,
                                        // The gapless next track is LOCAL playback,
                                        // so it resolves against the same capped
                                        // tier as the audible play (Slint twin:
                                        // playback.rs:5404). Prefetching at a
                                        // higher tier than the funnel would request
                                        // is exactly how a quality-blind cache
                                        // leaks an uncapped stream to the DAC.
                                        quality,
                                        off.as_deref(),
                                        sink.as_ref(),
                                    )
                                    .await
                                {
                                    if !gapless_edge_is_current(&runtime, track_id, next_id).await {
                                        log::info!(
                                        "[qbz-qt] [GAPLESS] discarded stale track {next_id} after predecessor {track_id} moved or its queue edge changed"
                                    );
                                        return;
                                    }
                                    let player = runtime.core().player();
                                    match player.play_next(data, next_id) {
                                        Ok(()) => log::info!(
                                            "[qbz-qt] [GAPLESS] queued track {next_id} for gapless"
                                        ),
                                        Err(e) => log::warn!(
                                            "[qbz-qt] [GAPLESS] play_next {next_id} failed: {e}"
                                        ),
                                    }
                                }
                            });
                            if let Some(stale) = gapless_fetch_task.replace(task) {
                                stale.abort();
                            }
                        } else if next.id != track_id
                            && gapless_source(&next) == GaplessSource::Local
                        {
                            // Source-owned gapless. `is_local` means "does not use
                            // Qobuz's tier walk", NOT "has a filesystem path": it
                            // also covers Plex/Jellyfin/Subsonic streams. Resolve
                            // the same PlaybackTicket normal playback consumes and
                            // let the shared exhaustive matcher turn files, bytes,
                            // DSD and remote streams into `play_next`.
                            gapless_requested_for = track_id;
                            let runtime = runtime.clone();
                            let task = spawn_owner_playback_task(async move {
                                let Some(_owner_action) =
                                    begin_owner_action_exact(owner_token).await
                                else {
                                    return;
                                };
                                let next_id = next.id;
                                match crate::audible_qt::queue_gapless_successor(
                                    &runtime, track_id, &next,
                                )
                                .await
                                {
                                    Ok(true) => log::info!(
                                    "[qbz-qt] [GAPLESS] queued source track {next_id} for gapless"
                                ),
                                    Ok(false) => log::debug!(
                                    "[qbz-qt] [GAPLESS] source pre-queue {next_id} not applicable"
                                ),
                                    Err(e) => log::warn!(
                                        "[qbz-qt] [GAPLESS] source pre-queue {next_id} failed: {e}"
                                    ),
                                }
                            });
                            if let Some(stale) = gapless_fetch_task.replace(task) {
                                stale.abort();
                            }
                        }
                    }
                }
            }

            // --- QConnect: outbound renderer state report -----------------
            // (contract §6.2 — Slint playback.rs:5676-5710, placed after the
            // position push / before the end-of-track edge exactly like the
            // reference.) When QBZ is the ACTIVE LOCAL renderer (controlled by
            // a remote controller like the iOS app), report our playback state
            // so the controller's seek bar + current-track follow. Mirrors the
            // official client: position/duration in MILLISECONDS (the Qt poll
            // position is SECONDS — ×1000 at the call), ~2s periodic while
            // playing + an immediate report on every transition. The service
            // self-gates on is_local_renderer_active (no-op when we're a peer
            // controller or not connected) and resolves the queue_item_ids —
            // no extra gates.
            report_tick = report_tick.wrapping_add(1);
            let report_track_id = qconnect_app::qconnect_report_track_id(&event);
            if report_track_id != 0 {
                let transition = report_track_id != last_reported_track_id
                    || is_playing != last_reported_playing
                    || event.buffer_state != last_reported_buffer_state;
                let buffer_active = matches!(
                    event.buffer_state,
                    qbz_player::player::PlaybackBufferState::InitialBuffering
                        | qbz_player::player::PlaybackBufferState::Underrun
                );
                let periodic = (is_playing || buffer_active)
                    && report_tick % QCONNECT_REPORT_EVERY_N_TICKS == 0;
                if transition || periodic {
                    if let Some(svc) = crate::qconnect_qt::service() {
                        let playing_state =
                            qconnect_app::renderer_playing_state(is_playing, event.buffer_state);
                        let position_ms = (position as i64) * 1000;
                        let duration_ms = (duration as i64) * 1000;
                        svc.report_playback_state(
                            playing_state,
                            position_ms,
                            duration_ms,
                            report_track_id,
                            event.buffer_state,
                        )
                        .await;
                        // On a track change, also reconcile the session queue:
                        // if the user started a new album/playlist on QBZ,
                        // push it so the controller follows. Self-gates +
                        // echo-suppresses.
                        if transition {
                            svc.sync_local_queue_if_changed().await;
                        }
                    }
                    last_reported_track_id = report_track_id;
                    last_reported_playing = is_playing;
                    last_reported_buffer_state = event.buffer_state;
                }
            }

            // --- End-of-track / source-failure recovery -------------------
            // Keep the sampled predicate for source types that do not publish
            // a stronger signal. CMAF and engine-empty recovery use durable
            // generations, so a playing pulse shorter than this 1 s poll is
            // still observed exactly once.
            let sampled_track_end = was_playing
                && !is_playing
                && last_track_id != 0
                && (track_id == 0 || track_id == last_track_id)
                && duration > 0
                && seen_position + 2 >= duration;
            let explicit_recovery_edge = event.engine_empty_generation
                != seen_engine_empty_generation
                || event.source_failure_generation != seen_source_failure_generation;
            let queue_track_id =
                if owner_snapshot.is_some() && (explicit_recovery_edge || sampled_track_end) {
                    runtime
                        .core()
                        .current_track()
                        .await
                        .map(|t| t.id)
                        .unwrap_or(0)
                } else {
                    0
                };
            let recovery_cause = playback_recovery_cause(
                &event,
                queue_track_id,
                seen_engine_empty_generation,
                seen_source_failure_generation,
                sampled_track_end,
            );
            // Consume every observed generation, including a stale edge whose
            // track id no longer matches after a manual play. It must not wake
            // up again if that id appears later in the session.
            seen_engine_empty_generation = event.engine_empty_generation;
            seen_source_failure_generation = event.source_failure_generation;

            if let Some(recovery_cause) = recovery_cause {
                let Some(owner_snapshot) = owner_snapshot.as_ref() else {
                    // The stamped delegated engine owns guest advancement.
                    // Consume the sampled edge locally without logging,
                    // mutating the cursor, or retrying it on every poll tick.
                    seen_position = position;
                    was_playing = is_playing;
                    last_owner_token = observed_owner_token;
                    continue;
                };
                match recovery_cause {
                    PlaybackRecoveryCause::SourceFailure => {
                        log::warn!(
                            "[qbz-qt] recovery: source retries exhausted for track {queue_track_id}; continuing the queue"
                        );
                        crate::listen_log_qt::on_error(owner_snapshot).await;
                    }
                    PlaybackRecoveryCause::EngineEmpty => {
                        log::info!(
                            "[qbz-qt] recovery: engine empty for track {queue_track_id} with no gapless successor; continuing the queue"
                        );
                        crate::listen_log_qt::on_natural_end(owner_snapshot).await;
                    }
                    PlaybackRecoveryCause::SampledNaturalEnd => {
                        crate::listen_log_qt::on_natural_end(owner_snapshot).await;
                    }
                }

                if let Some(task) = gapless_fetch_task.take() {
                    if !task.is_finished() {
                        log::info!(
                            "[qbz-qt] [GAPLESS] aborting unfinished successor fetch during recovery"
                        );
                        task.abort();
                    }
                }

                // Seed for InfiniteRadio, read BEFORE anything moves the
                // cursor: the track that just ended is still the current one.
                let ended_track_id = queue_track_id;
                // Stop-after-this-song: if the track that just ended carries
                // the marker, HALT here — do not advance, do not refill. The
                // queue stays intact and the finished track stays parked in
                // now-playing, so pressing play resumes from where the user
                // left off. `consume_stop_after_if` is one-shot (it clears the
                // marker), and this runs AHEAD of repeat/shuffle exactly as
                // the reference does (playback.rs:5749-5772) — behind them the
                // marker would lose to a repeat-one and never fire.
                if ended_track_id != 0 && runtime.core().consume_stop_after_if(ended_track_id).await
                {
                    if let Err(e) = runtime.core().pause() {
                        log::warn!("[qbz-qt] stop-after: pause failed: {e}");
                    }
                    last_track_id = 0;
                    seen_position = 0;
                    gapless_requested_for = 0;
                    crate::now_playing::set_playing(false);
                    // Republish so the row drops its CircleStop in the same
                    // frame the marker is consumed.
                    crate::queue_qt::publish(&runtime).await;
                    // The tail-of-loop bookkeeping, inlined because this arm
                    // returns to the top early and must NOT run
                    // `seen_position = position` (the reset above is the
                    // point). Dropping the Discord push here would leave the
                    // presence saying "playing" after the timer stopped us. The
                    // tray tooltip's middle-click hint has the identical
                    // problem — it would keep saying "Middle-click to pause"
                    // after the stop-after timer fired — so its push is a
                    // sibling here, exactly as in the reference, where tray
                    // (playback.rs:5719-5722) and Discord (`:5734`) share one
                    // `if`. THIS IS ONE OF TWO `is_playing != was_playing`
                    // BLOCKS; a push in only one is the silent half-port
                    // (§5.6, AC-27).
                    if is_playing != was_playing {
                        if let Some(t) = crate::tray_qt::handle() {
                            t.set_playing(is_playing);
                        }
                        // OS media controls, inserted between the tray and
                        // Discord exactly as in the reference, whose single
                        // block is tray (playback.rs:5720-5722), MPRIS
                        // (`:5723-5730`), Discord (`:5734`). Carries the live
                        // position, so this transition is also the only thing
                        // that moves the widget's Position — nothing per tick.
                        // SIBLING OF THE BLOCK AT THE LOOP TAIL: a push in only
                        // one of the two is the silent half-port.
                        crate::media_controls_qt::push_playback_state(is_playing, position);
                        crate::integrations_qt::discord_push_observed(
                            &runtime,
                            owner_snapshot.token(),
                            track_id,
                        );
                    }
                    was_playing = is_playing;
                    last_owner_token = observed_owner_token;
                    continue;
                }
                last_track_id = 0;
                // The skip-unavailable walk is ported: a failed play inside
                // `play_queue_track` hands off to `auto_skip_unavailable`
                // (PARITY-DEBT #2).
                if let Some(track) = runtime.core().next_track().await {
                    let next_id = track.id;
                    // Repeat-one advances to the same id; there the ended id IS
                    // the next one and nothing can be told apart, so the old
                    // behaviour stands.
                    stale_after_advance = if next_id != ended_track_id {
                        ended_track_id
                    } else {
                        0
                    };
                    log::info!("[qbz-qt] poll: advancing to {next_id}");
                    play_queue_track(&runtime, next_id, 0).await;
                } else if try_infinite_refill(&runtime, ended_track_id).await {
                    // InfiniteRadio started a fresh smart radio; it replaced
                    // the queue and published on its way in.
                } else {
                    log::info!("[qbz-qt] poll: queue finished");
                    crate::now_playing::set_playing(false);
                }
            }

            seen_position = position;
            // Persist the live position every ~5s while playing so a crash keeps
            // a near-current resume point. The reference ticks at 450ms and uses
            // `% 11`; this loop ticks at 1 Hz, so the WALL-CLOCK equivalent is
            // `% 5` — tick-count throttles do not translate 1:1.
            save_pos_tick = save_pos_tick.wrapping_add(1);
            if is_playing && track_id != 0 && save_pos_tick % 5 == 0 {
                if owner_snapshot.is_some() {
                    qbz_app::session_persist::save_position(position);
                }
            }
            // Reflect play/pause into the tray tooltip on the TRANSITION only,
            // so the "Middle-click to pause/play" hint stays correct without
            // spamming the updater channel every tick (§7-M12). The second of
            // the two `is_playing != was_playing` blocks — see the sibling in
            // the stop-after arm above.
            if is_playing != was_playing {
                if let Some(t) = crate::tray_qt::handle() {
                    t.set_playing(is_playing);
                }
                // OS media controls — the SECOND of the two blocks, see the
                // sibling in the stop-after arm above. Same order as the
                // reference: tray, MPRIS, Discord.
                crate::media_controls_qt::push_playback_state(is_playing, position);
                if let Some(owner_snapshot) = owner_snapshot.as_ref() {
                    crate::integrations_qt::discord_push_observed(
                        &runtime,
                        owner_snapshot.token(),
                        track_id,
                    );
                }
            }
            was_playing = is_playing;
            last_owner_token = observed_owner_token;
        }
    });
}

// ---------------------------------------------------------------------------
// Tests (no Qt, engine, audio device or session)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    #[test]
    fn explicit_pause_never_starts_a_paused_or_empty_player() {
        for playing in [false, true] {
            for pending in [false, true] {
                let ignored = super::ignore_playing_request(Some(false), playing, pending);
                assert_eq!(ignored, !playing && !pending);
                // A non-ignored Pause must either pause audio or cancel a load.
                assert!(ignored || playing || pending);
            }
        }
        // Repeating Play while the first request is loading must not cancel it.
        assert!(super::ignore_playing_request(Some(true), false, true));
        assert!(super::ignore_playing_request(Some(true), true, false));
        assert!(!super::ignore_playing_request(Some(true), false, false));
        for playing in [false, true] {
            for pending in [false, true] {
                assert!(!super::ignore_playing_request(None, playing, pending));
            }
        }
    }

    use super::{
        begin_resolved_play, cancel_owner_playback_tasks, capture_owner_scoped_snapshot,
        filter_queue_with, gapless_edge_matches, gapless_successor_changed,
        owner_generation_changed, playback_recovery_cause, prefetch_track_ids,
        purchase_quality_badge, quality_badge_from, reconcile_device_cap,
        resolved_play_is_current, should_stream_gapless_successor, source_change_targets_current,
        spawn_owner_playback_task, stream_error_text, xorshift_shuffle_seeded,
        OwnerActionObservation, OwnerActionToken, OwnerScopedSnapshot, PlaybackRecoveryCause,
        PrefetchCandidate, PrefetchTrackEdge,
    };
    use qbz_models::{Quality, QualityLimit, QueueTrack};
    use qbz_player::PlaybackEvent;
    use qbz_source::QualityHint;
    use std::sync::Arc;

    fn remote(id: u64) -> PrefetchCandidate {
        PrefetchCandidate {
            id,
            is_local: false,
        }
    }

    fn local(id: u64) -> PrefetchCandidate {
        PrefetchCandidate { id, is_local: true }
    }

    #[test]
    fn a_new_resolved_start_supersedes_older_policy_work() {
        let old = begin_resolved_play();
        assert!(resolved_play_is_current(old));
        let new = begin_resolved_play();
        assert!(!resolved_play_is_current(old));
        assert!(resolved_play_is_current(new));
    }

    fn playback_event(track_id: u64, is_playing: bool) -> PlaybackEvent {
        PlaybackEvent {
            is_playing,
            position: 0,
            duration: 0,
            track_id,
            volume: 1.0,
            hardware_volume_active: false,
            sample_rate: None,
            bit_depth: None,
            shuffle: None,
            repeat: None,
            normalization_gain: None,
            gapless_ready: false,
            gapless_next_track_id: 0,
            bit_perfect_mode: None,
            buffer_progress: None,
            buffer_state: qbz_player::player::PlaybackBufferState::Idle,
            buffer_track_id: 0,
            engine_empty_generation: 0,
            engine_empty_track_id: 0,
            source_failure_generation: 0,
            source_failure_track_id: 0,
        }
    }

    #[test]
    fn owner_observation_precedes_snapshot_and_fence_does_not_consume_it() {
        use std::cell::RefCell;

        let order = RefCell::new(Vec::new());
        let captured = capture_owner_scoped_snapshot(
            || {
                order.borrow_mut().push("observe");
                OwnerActionObservation::Delegated
            },
            || {
                order.borrow_mut().push("read");
                41_u64
            },
        );
        assert!(matches!(
            captured,
            OwnerScopedSnapshot::Captured {
                owner_action: None,
                snapshot: 41
            }
        ));
        assert_eq!(*order.borrow(), ["observe", "read"]);

        order.borrow_mut().clear();
        let fenced = capture_owner_scoped_snapshot(
            || {
                order.borrow_mut().push("observe");
                OwnerActionObservation::Fenced
            },
            || {
                order.borrow_mut().push("read");
                99_u64
            },
        );
        assert!(matches!(fenced, OwnerScopedSnapshot::Fenced));
        assert_eq!(*order.borrow(), ["observe"]);

        let first = OwnerActionToken::BeforeService;
        assert!(owner_generation_changed(Some(first), None));
        assert!(!owner_generation_changed(Some(first), Some(first)));
        assert!(!owner_generation_changed(None, Some(first)));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn owner_background_task_is_cancelable_before_it_can_start_io() {
        use std::sync::atomic::{AtomicBool, Ordering};

        cancel_owner_playback_tasks();
        let ran = Arc::new(AtomicBool::new(false));
        let observed = Arc::clone(&ran);
        let task = spawn_owner_playback_task(async move {
            observed.store(true, Ordering::Release);
        });
        cancel_owner_playback_tasks();
        let _ = task.await;
        assert!(!ran.load(Ordering::Acquire));

        let completed = Arc::new(AtomicBool::new(false));
        let observed = Arc::clone(&completed);
        spawn_owner_playback_task(async move {
            observed.store(true, Ordering::Release);
        })
        .await
        .unwrap();
        assert!(completed.load(Ordering::Acquire));
    }

    #[test]
    fn engine_empty_edge_survives_a_missed_playing_pulse() {
        let mut event = playback_event(41, false);
        event.engine_empty_generation = 1;
        event.engine_empty_track_id = 41;

        assert_eq!(
            playback_recovery_cause(&event, 41, 0, 0, false),
            Some(PlaybackRecoveryCause::EngineEmpty)
        );
    }

    #[test]
    fn source_failure_recovers_a_track_that_never_started() {
        let mut event = playback_event(0, false);
        event.source_failure_generation = 1;
        event.source_failure_track_id = 41;

        assert_eq!(
            playback_recovery_cause(&event, 41, 0, 0, false),
            Some(PlaybackRecoveryCause::SourceFailure)
        );
    }

    #[test]
    fn stale_recovery_edge_cannot_skip_a_new_queue_track() {
        let mut event = playback_event(42, false);
        event.source_failure_generation = 3;
        event.source_failure_track_id = 41;
        event.engine_empty_generation = 2;
        event.engine_empty_track_id = 41;

        assert_eq!(playback_recovery_cause(&event, 42, 1, 2, false), None);
    }

    #[test]
    fn late_gapless_rescue_consumes_engine_edge_without_advancing() {
        let mut event = playback_event(41, true);
        event.engine_empty_generation = 1;
        event.engine_empty_track_id = 41;
        event.gapless_next_track_id = 42;

        assert_eq!(playback_recovery_cause(&event, 41, 0, 0, false), None);
    }

    fn queue_track() -> QueueTrack {
        QueueTrack {
            id: 1,
            title: String::new(),
            version: None,
            artist: String::new(),
            album: String::new(),
            album_version: None,
            duration_secs: 0,
            artwork_url: None,
            hires: false,
            bit_depth: None,
            sample_rate: None,
            is_local: true,
            album_id: None,
            artist_id: None,
            streamable: true,
            source: Some("navidrome".to_string()),
            parental_warning: false,
            source_item_id_hint: None,
            context_kind: None,
            context_id: None,
            isrc: None,
            recording_mbid: None,
        }
    }

    // --- Source-backed NPB quality --------------------------------------

    #[test]
    fn mp3_source_hint_is_not_mislabeled_as_cd() {
        let badge = quality_badge_from(
            &queue_track(),
            Some(QualityHint {
                bit_depth: None,
                sample_rate_khz: Some(44.1),
                format: "MP3",
            }),
        );
        assert_eq!(badge, ("mp3".to_string(), "44.1 kHz · MP3".to_string()));
    }

    #[test]
    fn lossless_source_rate_can_fill_thin_queue_metadata() {
        let badge = quality_badge_from(
            &queue_track(),
            Some(QualityHint {
                bit_depth: None,
                sample_rate_khz: Some(48.0),
                format: "FLAC",
            }),
        );
        assert_eq!(badge, ("cd".to_string(), "16-bit / 48 kHz".to_string()));
    }

    #[test]
    fn queue_metadata_wins_over_the_source_fallback() {
        let mut row = queue_track();
        row.bit_depth = Some(24);
        row.sample_rate = Some(96.0);
        let badge = quality_badge_from(
            &row,
            Some(QualityHint {
                bit_depth: Some(16),
                sample_rate_khz: Some(44.1),
                format: "FLAC",
            }),
        );
        assert_eq!(badge, ("hires".to_string(), "24-bit / 96 kHz".to_string()));
    }

    #[test]
    fn purchase_format_is_the_npb_badge_source() {
        assert_eq!(
            purchase_quality_badge(56),
            Some(("dsd", "DSD128", Some(1), Some(5_644_800.0)))
        );
        assert_eq!(
            purchase_quality_badge(7),
            Some(("hires", "24-bit / 96 kHz", Some(24), Some(96.0)))
        );
        assert_eq!(
            purchase_quality_badge(5),
            Some(("mp3", "320 kbps · MP3", None, None))
        );
        assert_eq!(purchase_quality_badge(999), None);
    }

    #[test]
    fn source_change_only_targets_the_audible_track_in_that_album() {
        let mut track = queue_track();
        track.id = 41;
        track.album_id = Some("album-a".to_string());
        let mut event = playback_event(41, true);
        event.position = 73;

        assert!(source_change_targets_current("album-a", &track, &event));
        assert!(!source_change_targets_current("album-b", &track, &event));

        event.track_id = 42;
        assert!(!source_change_targets_current("album-a", &track, &event));
        event.track_id = 41;
        event.is_playing = false;
        assert!(!source_change_targets_current("album-a", &track, &event));
    }

    // --- #688: immediate two-successor L1/L2 warming ---------------------

    #[test]
    fn streaming_only_on_plans_zero_prefetches() {
        assert!(prefetch_track_ids(true, false, 2, &[remote(2), remote(3)]).is_empty());
    }

    #[test]
    fn streaming_only_off_plans_at_most_two_successors() {
        assert_eq!(
            prefetch_track_ids(false, false, 2, &[remote(2), remote(3), remote(4)]),
            vec![2, 3]
        );
        assert_eq!(prefetch_track_ids(false, false, 2, &[remote(9)]), vec![9]);
    }

    #[test]
    fn local_successors_are_never_prefetched() {
        // The policy inspects the queue's immediate next-two snapshot. It does
        // not skip across the local row and turn the third row into a hidden
        // third-successor warmup.
        assert_eq!(
            prefetch_track_ids(false, false, 2, &[remote(2), local(3), remote(4)]),
            vec![2]
        );
    }

    #[test]
    fn a_repeated_track_edge_does_not_trigger_twice() {
        let mut edge = PrefetchTrackEdge::default();
        assert!(edge.observe(41));
        assert!(!edge.observe(41));
        assert!(!edge.observe(41));
    }

    #[test]
    fn a_gapless_new_current_edge_prefetches_that_tracks_successors() {
        let mut edge = PrefetchTrackEdge::default();
        assert!(edge.observe(41), "initial audible track must be an edge");
        assert!(edge.observe(42), "gapless handoff must surface a new edge");
        assert_eq!(
            prefetch_track_ids(false, false, 2, &[remote(43), remote(44)]),
            vec![43, 44]
        );
        assert!(!edge.observe(42), "republishing the handoff must dedupe");
    }

    #[test]
    fn throttle_cap_zero_plans_zero_downloads() {
        assert!(prefetch_track_ids(false, false, 0, &[remote(2), remote(3)]).is_empty());
    }

    #[test]
    fn streaming_only_cold_successor_uses_incremental_gapless() {
        assert!(should_stream_gapless_successor(true, false, false, 1));
    }

    #[test]
    fn cached_successor_uses_file_handoff_and_offline_keeps_its_resolver() {
        assert!(should_stream_gapless_successor(true, true, false, 1));
        assert!(should_stream_gapless_successor(false, true, false, 0));
        assert!(should_stream_gapless_successor(false, false, false, 1));
        assert!(!should_stream_gapless_successor(true, false, true, 1));
    }

    #[test]
    fn adaptive_cap_zero_blocks_incremental_gapless_download() {
        assert!(!should_stream_gapless_successor(true, false, false, 0));
    }

    #[test]
    fn completed_gapless_fetch_only_matches_its_live_queue_edge() {
        assert!(gapless_edge_matches(1, 2, 1, 0, None, Some(2)));
        assert!(!gapless_edge_matches(1, 2, 2, 0, None, Some(2)));
        assert!(!gapless_edge_matches(1, 2, 1, 9, None, Some(2)));
        assert!(!gapless_edge_matches(1, 2, 1, 0, Some(1), Some(2)));
        assert!(!gapless_edge_matches(1, 2, 1, 0, None, Some(3)));
        assert!(!gapless_edge_matches(1, 1, 1, 0, None, Some(1)));
    }

    #[test]
    fn shuffle_restarts_only_when_an_armed_successor_became_stale() {
        let mut event = playback_event(1, true);
        assert!(!gapless_successor_changed(&event, Some(2)));

        event.gapless_next_track_id = 2;
        assert!(!gapless_successor_changed(&event, Some(2)));
        assert!(gapless_successor_changed(&event, Some(3)));
        assert!(gapless_successor_changed(&event, None));

        event.track_id = 0;
        assert!(!gapless_successor_changed(&event, Some(3)));
    }

    // --- #638 fix 3: the request tier reconciles preference against the
    // detected local device cap. -------------------------------------------

    #[test]
    fn with_no_cap_the_preference_governs_and_names_itself() {
        assert_eq!(
            reconcile_device_cap(Quality::UltraHiRes, None),
            (Quality::UltraHiRes, QualityLimit::None)
        );
        // Below Hi-Res+ with no cap, the cause is the preference — that is
        // what the badge tooltip says when nothing else is limiting.
        assert_eq!(
            reconcile_device_cap(Quality::HiRes, None),
            (Quality::HiRes, QualityLimit::Preference)
        );
    }

    #[test]
    fn a_cap_below_the_preference_clamps_and_blames_the_device() {
        assert_eq!(
            reconcile_device_cap(Quality::UltraHiRes, Some(Quality::Lossless)),
            (Quality::Lossless, QualityLimit::LocalDeviceCap)
        );
        assert_eq!(
            reconcile_device_cap(Quality::UltraHiRes, Some(Quality::HiRes)),
            (Quality::HiRes, QualityLimit::LocalDeviceCap)
        );
    }

    /// THE TIE ARM. A 96 kHz DAC with the preference already at Hi-Res
    /// requests the same tier either way, so the clamp is invisible — but the
    /// CAUSE is not, and the device is the more specific and more surprising
    /// of the two. Drop this arm and the tooltip tells the user their
    /// preference is the ceiling while the real ceiling is their hardware.
    #[test]
    fn a_cap_equal_to_the_preference_still_blames_the_device_below_hi_res_plus() {
        assert_eq!(
            reconcile_device_cap(Quality::HiRes, Some(Quality::HiRes)),
            (Quality::HiRes, QualityLimit::LocalDeviceCap)
        );
        assert_eq!(
            reconcile_device_cap(Quality::Lossless, Some(Quality::Lossless)),
            (Quality::Lossless, QualityLimit::LocalDeviceCap)
        );
    }

    /// The tie at the TOP is the exception: Hi-Res+ capped at Hi-Res+ is not a
    /// limit at all, and claiming the device limits you there would be a lie.
    #[test]
    fn a_tie_at_hi_res_plus_is_not_a_limit() {
        assert_eq!(
            reconcile_device_cap(Quality::UltraHiRes, Some(Quality::UltraHiRes)),
            (Quality::UltraHiRes, QualityLimit::None)
        );
    }

    /// A cap ABOVE the preference must not raise the request. The user asked
    /// for less; a capable DAC is not permission to spend their bandwidth.
    #[test]
    fn a_cap_above_the_preference_never_raises_it() {
        assert_eq!(
            reconcile_device_cap(Quality::Lossless, Some(Quality::UltraHiRes)),
            (Quality::Lossless, QualityLimit::Preference)
        );
    }

    /// THE #638 TRAP, pinned as a source audit because no runtime test can
    /// reach it: `current_quality()` is the UNCAPPED preference, and exactly
    /// two places may read it — the capped resolver in this module, and
    /// `cast_qt::effective_cast_quality`, which must never see the local cap
    /// (the local DAC is not in a cast's signal path; capping it there is
    /// bug #638 in reverse — a 48 kHz DAC silently capping a 192 kHz DLNA
    /// renderer).
    ///
    /// A new play path that reaches for `current_quality()` instead of the
    /// funnel fails here, by name, instead of shipping an uncapped request
    /// that no static gate and no smoke of an unrelated view would catch.
    #[test]
    fn only_the_resolver_and_the_cast_path_read_the_uncapped_preference() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut offenders: Vec<String> = Vec::new();
        let mut walk = vec![src];
        while let Some(dir) = walk.pop() {
            for entry in std::fs::read_dir(&dir).expect("read src dir") {
                let path = entry.expect("dir entry").path();
                if path.is_dir() {
                    walk.push(path);
                    continue;
                }
                if path.extension().and_then(|e| e.to_str()) != Some("rs") {
                    continue;
                }
                let name = path.file_name().unwrap().to_string_lossy().to_string();
                let body = std::fs::read_to_string(&path).expect("read source");
                // Production code only — a test module naming the function
                // (this one does, twice) is not a call site.
                let body = match body.find("\n#[cfg(test)]") {
                    Some(cut) => &body[..cut],
                    None => &body[..],
                };
                for (i, line) in body.lines().enumerate() {
                    // Skip the definition, doc comments and ordinary comments —
                    // this audits CALLS, not prose about them.
                    let t = line.trim_start();
                    if t.starts_with("//")
                        || t.starts_with("fn ")
                        || t.starts_with("pub(crate) fn ")
                    {
                        continue;
                    }
                    if line.contains("current_quality()") {
                        offenders.push(format!("{name}:{}", i + 1));
                    }
                }
            }
        }
        // playback_qt.rs: the one read inside `local_playback_quality`.
        // cast_qt.rs: `effective_cast_quality`, which MUST stay uncapped.
        let allowed = ["playback_qt.rs:", "cast_qt.rs:"];
        let unexpected: Vec<&String> = offenders
            .iter()
            .filter(|o| !allowed.iter().any(|a| o.starts_with(a)))
            .collect();
        assert!(
            unexpected.is_empty(),
            "these read the UNCAPPED streaming preference instead of going through the play \
             funnel / `local_playback_quality` — see #638 fix 3: {unexpected:?}"
        );
        // And the two allowed files read it exactly once each: a second read
        // in playback_qt.rs means the funnel was bypassed again.
        assert_eq!(
            offenders.len(),
            2,
            "expected exactly two reads of `current_quality()` (the resolver + the cast path), \
             found: {offenders:?}"
        );
    }

    // --- PARITY-DEBT #17: the shuffle flag reorders, it does not switch mode
    //
    // The permutation itself is part of the contract being ported: the
    // reference walks Fisher–Yates DOWNWARD (`for i in (1..len).rev()`) over
    // an xorshift64 stream seeded once. A different loop direction or a
    // different mix order is still "random" and still passes a smoke test, so
    // it is pinned here against an independently-computed expectation.

    /// Reference permutation for seed 0x2545F4914F6CDD1D over 0..8, computed
    /// from the algorithm at `playback.rs:3131-3144`.
    #[test]
    fn xorshift_shuffle_matches_the_reference_permutation() {
        let mut items: Vec<u32> = (0..8).collect();
        xorshift_shuffle_seeded(&mut items, 0x2545_F491_4F6C_DD1D);
        assert_eq!(items, vec![1, 0, 3, 2, 6, 5, 4, 7]);
    }

    /// A second seed, so the test pins the PRNG and not one lucky constant.
    #[test]
    fn xorshift_shuffle_is_seed_driven() {
        let mut items: Vec<u32> = (0..5).collect();
        xorshift_shuffle_seeded(&mut items, 12345);
        assert_eq!(items, vec![3, 0, 2, 1, 4]);
    }

    /// The reference's loop never runs for 0 or 1 elements — a one-track label
    /// Shuffle must not panic on the `1..len` range.
    #[test]
    fn xorshift_shuffle_is_a_no_op_below_two_elements() {
        let mut one = vec![7u32];
        xorshift_shuffle_seeded(&mut one, 42);
        assert_eq!(one, vec![7]);
        let mut none: Vec<u32> = vec![];
        xorshift_shuffle_seeded(&mut none, 42);
        assert!(none.is_empty());
    }

    /// The seed is OR-ed with 1 the way the reference does, so a zero clock
    /// reading cannot park xorshift64 on its fixed point (which would leave
    /// the list in its original order — a "shuffle" that never shuffles).
    #[test]
    fn a_zero_seed_still_shuffles() {
        let mut items: Vec<u32> = (0..8).collect();
        xorshift_shuffle_seeded(&mut items, 0);
        assert_ne!(items, (0..8).collect::<Vec<u32>>());
    }

    /// Whatever the seed, shuffling is a permutation: nothing is lost and
    /// nothing is duplicated (the queue the user gets is the queue they asked
    /// for, reordered).
    #[test]
    fn xorshift_shuffle_is_a_permutation() {
        for seed in [1u64, 2, 3, 999, u64::MAX] {
            let mut items: Vec<u32> = (0..64).collect();
            xorshift_shuffle_seeded(&mut items, seed);
            items.sort_unstable();
            assert_eq!(items, (0..64).collect::<Vec<u32>>());
        }
    }

    // --- PARITY-DEBT #7: the stream-error toast text ----------------------
    //
    // Tests run with the default (English) catalog, so `qbz_i18n::t` returns
    // the msgid — which is exactly the string the reference builds.

    /// Outside a sandbox the raw engine error is shown, prefixed.
    #[test]
    fn stream_error_outside_a_sandbox_keeps_the_raw_message() {
        assert_eq!(
            stream_error_text("ALSA: device or resource busy", false),
            "Audio output error: ALSA: device or resource busy"
        );
    }

    /// Inside a sandbox an ALSA-shaped failure is rewritten into the
    /// actionable sentence (`playback.rs:5105-5111`).
    #[test]
    fn stream_error_inside_a_sandbox_rewrites_alsa_failures() {
        let expected = "Direct ALSA device access is blocked inside Flatpak/Snap — switch the audio backend to PipeWire or System Default";
        assert_eq!(stream_error_text("ALSA open failed", true), expected);
        // The match is case-insensitive and also fires on the device string.
        assert_eq!(stream_error_text("cannot open hw:1,0", true), expected);
        assert_eq!(stream_error_text("alsa: no such device", true), expected);
    }

    /// A sandboxed failure that is NOT ALSA-shaped keeps the raw message —
    /// the rewrite must not swallow an unrelated cause.
    #[test]
    fn stream_error_inside_a_sandbox_keeps_non_alsa_failures() {
        assert_eq!(
            stream_error_text("HTTP 403 from the CDN", true),
            "Audio output error: HTTP 403 from the CDN"
        );
    }

    /// Drop the odd numbers; `start` must follow the CLICKED item, not its old
    /// index. Without the remap the core would start on a different track than
    /// the one the user clicked — silently, and only when something was blocked.
    #[test]
    fn start_index_follows_the_clicked_track() {
        let (kept, start, _) = filter_queue_with(
            vec![0, 1, 2, 3, 4],
            Some(2),
            |n| n % 2 == 1,
            |_| false,
            |_| false,
        );
        assert_eq!(kept, vec![0, 2, 4]);
        assert_eq!(start, Some(1)); // the 2 moved from index 2 to index 1
    }

    /// Clicking a row that is itself blocked lands on the next survivor.
    #[test]
    fn a_blocked_clicked_row_falls_forward() {
        let (kept, start, _) = filter_queue_with(
            vec![0, 1, 2, 3],
            Some(1),
            |n| n % 2 == 1,
            |_| false,
            |_| false,
        );
        assert_eq!(kept, vec![0, 2]);
        assert_eq!(start, Some(1)); // the 1 was dropped, so start on the 2
    }

    /// The whole tail blocked: start on the LAST survivor, never past the end
    /// (an out-of-range index is what makes the core start nothing at all).
    #[test]
    fn a_fully_blocked_tail_clamps_to_the_last_survivor() {
        let (kept, start, _) = filter_queue_with(
            vec![0, 1, 3, 5],
            Some(2),
            |n| n % 2 == 1,
            |_| false,
            |_| false,
        );
        assert_eq!(kept, vec![0]);
        assert_eq!(start, Some(0));
    }

    /// Everything blocked: an empty queue carries NO start index.
    #[test]
    fn everything_blocked_yields_no_start() {
        let (kept, start, _) =
            filter_queue_with(vec![1, 3, 5], Some(1), |n| n % 2 == 1, |_| false, |_| false);
        assert!(kept.is_empty());
        assert_eq!(start, None);
    }

    /// Nothing blocked: the queue and the index are untouched — the path every
    /// user without a blacklist takes.
    #[test]
    fn nothing_blocked_is_identity() {
        let (kept, start, _) =
            filter_queue_with(vec![0, 2, 4], Some(2), |_| false, |_| false, |_| false);
        assert_eq!(kept, vec![0, 2, 4]);
        assert_eq!(start, Some(2));
    }

    /// Acceptance §8-5: a build containing dead tracks drops them AND re-maps
    /// the start index to the track the user actually clicked. Same arithmetic
    /// as the blacklist, different predicate — which is why the helper took a
    /// dedicated closure instead of a rewrite.
    #[test]
    fn unavailable_rows_drop_and_remap_the_start_index() {
        // Rows 1 and 3 are "pulled"; the user clicked row 4.
        let (kept, start, dropped) = filter_queue_with(
            vec![0, 1, 2, 3, 4],
            Some(4),
            |_| false,
            |n| *n == 1 || *n == 3,
            |_| false,
        );
        assert_eq!(kept, vec![0, 2, 4]);
        assert_eq!(start, Some(2)); // the 4 moved from index 4 to index 2
        assert_eq!(dropped.unavailable, 2);
        assert_eq!(dropped.blacklisted, 0);
    }

    /// The two reasons are counted apart because only ONE of them speaks: the
    /// blacklist drop is what the user asked for, the unavailability drop is
    /// news. A single counter would force one policy on both.
    #[test]
    fn the_two_drop_reasons_are_counted_separately() {
        let (kept, _, dropped) = filter_queue_with(
            vec![0, 1, 2, 3],
            Some(0),
            |n| *n == 1,
            |n| *n == 2,
            |_| false,
        );
        assert_eq!(kept, vec![0, 3]);
        assert_eq!(dropped.blacklisted, 1);
        assert_eq!(dropped.unavailable, 1);
    }

    /// A row that is BOTH blocked and pulled counts as blacklisted, so no toast
    /// is raised for it: "1 track skipped" would tell the user about exactly the
    /// content the blacklist exists to keep off their screen.
    #[test]
    fn blacklist_wins_the_tie_and_stays_silent() {
        let (kept, _, dropped) =
            filter_queue_with(vec![0, 1], Some(0), |n| *n == 1, |n| *n == 1, |_| false);
        assert_eq!(kept, vec![0]);
        assert_eq!(dropped.blacklisted, 1);
        assert_eq!(dropped.unavailable, 0);
    }

    #[test]
    fn qconnect_unresolvable_rows_drop_and_remap_the_start_index() {
        let (kept, start, dropped) = filter_queue_with(
            vec![0, 1, 2, 3, 4],
            Some(3),
            |_| false,
            |_| false,
            |n| *n == 1 || *n == 3,
        );
        assert_eq!(kept, vec![0, 2, 4]);
        assert_eq!(start, Some(2));
        assert_eq!(dropped.qconnect_unresolvable, 2);
        assert_eq!(dropped.blacklisted, 0);
        assert_eq!(dropped.unavailable, 0);
    }

    #[test]
    fn song_card_near_fit_keeps_the_final_glyphs_instead_of_an_ellipsis() {
        // TextMetrics.width is a painted bounding box, while the line layout
        // consumes advanceWidth. Budgeting the former exactly turned
        // "Metallica" into "Metalli…" in a runway with visible spare space.
        let qml = include_str!("../qml/shell/SongCard.qml");
        assert!(qml.contains("artistMetrics.advanceWidth"));
        assert!(qml.contains("albumMetrics.advanceWidth"));
        assert!(qml.contains("a - qa <= near"));
        assert!(qml.contains("b - qb <= near"));
    }
}

#[cfg(test)]
mod offline_gapless_regressions {
    use super::*;

    #[tokio::test]
    async fn local_library_download_routes_to_offline_bytes_by_catalog_id() {
        use qbz_offline_cache::{OfflineCacheState, OfflineCacheStatus, TrackCacheInfo};
        let temp = tempfile::tempdir().unwrap();
        let offline = OfflineCacheState::new_empty();
        offline.init_at(temp.path()).await.unwrap();
        let file = temp.path().join("download.flac");
        let bytes = b"fLaC-regression-byte-source";
        std::fs::write(&file, bytes).unwrap();
        let row = crate::local_playback::local_queue_track(&qbz_library::LocalTrack {
            id: 4938,
            qobuz_track_id: Some(266725026),
            source: Some("qobuz_download".into()),
            file_path: file.display().to_string(),
            ..Default::default()
        });
        assert!(row.is_local);
        assert_eq!(row.source_item_id_hint.as_deref(), Some("4938"));
        assert_eq!(gapless_source(&row), GaplessSource::Download);
        {
            let guard = offline.db.lock().await;
            let db = guard.as_ref().unwrap();
            db.insert_track(
                &TrackCacheInfo {
                    track_id: row.id,
                    title: "Next".into(),
                    artist: "Fixture".into(),
                    album: None,
                    album_id: None,
                    duration_secs: 30,
                    quality: "hires".into(),
                    bit_depth: Some(24),
                    sample_rate: Some(192000.0),
                },
                file.to_str().unwrap(),
            )
            .unwrap();
            db.update_status(row.id, OfflineCacheStatus::Ready, None)
                .unwrap();
        }
        let resolved = qbz_core::offline_resolve::resolve_offline_bytes(row.id, &offline, None)
            .await
            .unwrap();
        assert_eq!(resolved, bytes);
        assert!(
            qbz_core::offline_resolve::resolve_offline_bytes(4938, &offline, None)
                .await
                .is_none()
        );
        std::fs::remove_file(file).unwrap();
        assert!(
            qbz_core::offline_resolve::resolve_offline_bytes(row.id, &offline, None)
                .await
                .is_none()
        );
    }

    fn queue_row(source: &str, is_local: bool) -> QueueTrack {
        let mut row = crate::local_playback::local_queue_track(&qbz_library::LocalTrack {
            id: 42, ..Default::default()
        });
        row.source = Some(source.into());
        row.is_local = is_local;
        row
    }

    #[test]
    fn files_purchases_and_servers_keep_their_source_route() {
        for source in [
            "user",
            "local",
            "ephemeral",
            "qobuz_purchase",
            "plex",
            "jellyfin",
            "subsonic",
            "navidrome",
        ] {
            let row = queue_row(source, true);
            assert_eq!(gapless_source(&row), GaplessSource::Local, "{source}");
        }
        for source in ["qobuz_download", "offline"] {
            let row = queue_row(source, true);
            assert_eq!(gapless_source(&row), GaplessSource::Download, "{source}");
        }
        assert_eq!(
            gapless_source(&queue_row("qobuz", false)),
            GaplessSource::Catalog
        );
    }
}
