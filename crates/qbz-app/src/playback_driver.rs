// crates/qbz-app/src/playback_driver.rs — the headless playback orchestrator.
//
// This re-hosts, as a PURE decision function plus a thin IO shell, the playback
// bookkeeping that today only exists inside the desktop's 450 ms poll loop
// (`crates/qbz/src/playback.rs::start_poll_loop`): end-of-track detection,
// auto-advance, gapless pre-queue, stop-after, seamless-transition cursor sync
// and the periodic session-position save. Every branch below cites the exact
// desktop line it mirrors — the desktop is the reference for tie-breaks, the
// unit tests pin the observable contract (01-architecture.md §3.2).
//
// Split of concerns:
//   * `plan_tick`      — side-effect-free: (state, event, queue, error) → actions
//   * `advance_state`  — the pure state-update rule the shell applies each tick
//   * `next_playable`  — bounded unstreamable skip-walk (mirrors
//                        `playback.rs::advance_to_playable`, capped at
//                        `MAX_OFFLINE_SKIPS`)
//   * `run_driver`     — the 450 ms IO shell that reads the player, calls
//                        `plan_tick`, and executes the resulting actions
//   * `advance_and_play` — the full advance ritual (skip-walk → play → prefetch
//                        → persist), reused verbatim by the CLI next/prev routes

use std::any::Any;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use qbz_core::{FrontendAdapter, QbzCore};
use qbz_models::{Quality, QueueTrack, RepeatMode};
use qbz_player::{PlaybackBufferState, PlaybackEvent};

use crate::session_store::{
    PersistedPlaybackSession, PersistedQueueTrack, PersistedSessionSnapshot,
    PersistedShellViewState,
};
use crate::shell::AppRuntime;

/// Poll cadence — the same 450 ms the desktop loop uses
/// (`playback.rs:4088`).
const TICK_MS: u64 = 450;

/// Session-position save cadence: every ~11 ticks ≈ 5 s
/// (`playback.rs:4306`, `save_pos_tick % 11 == 0`).
const SAVE_POSITION_EVERY_N_TICKS: u64 = 11;

/// QConnect report cadence while playing: every ~4 ticks ≈ 2 s
/// (`playback.rs:4069`, `QCONNECT_REPORT_EVERY_N_TICKS`).
const QCONNECT_REPORT_EVERY_N_TICKS: u64 = 4;

/// Bounded skip-walk ceiling for unavailable tracks (Tauri #467 parity;
/// `playback.rs:226`, `MAX_OFFLINE_SKIPS = 5`).
const MAX_OFFLINE_SKIPS: usize = 5;

/// A side effect the shell must perform this tick. Produced by [`plan_tick`],
/// executed by [`run_driver`]. Consumed by later tasks (T7 next/prev reuses the
/// advance ritual, T10 wires `ReportEdge`, T11 the settings reload).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriverAction {
    /// Reconcile the queue cursor to the id the engine is actually playing
    /// (a seamless gapless hand-off; `playback.rs:4340`).
    SyncCursorTo(u64),
    /// Pre-queue this upcoming track's bytes for a gapless transition
    /// (`playback.rs:4387`).
    ArmGapless(u64),
    /// A track started (by ANY entry: API, QConnect, CLI, natural advance or
    /// a seamless hand-off) — warm its successor into the player cache now,
    /// while the whole track duration is still ahead. Fired once per current
    /// track id. Without it the daemon only warmed after a gapped advance, so
    /// every edge after a seamless one was cold and had to fit a whole-file
    /// download inside the fixed 10 s gapless window (#699 follow-up, 1 GB Pi).
    WarmSuccessor,
    /// The current track ended and there is a next playable track — run the
    /// full advance ritual (`playback.rs:4743`).
    AdvanceAndPlay,
    /// The ended track was stop-after-marked — halt (pause), never advance
    /// (`playback.rs:4720`).
    PauseStopAfter,
    /// Persist the live position (throttled ~5 s; `playback.rs:4307`).
    SavePosition(u64),
    /// Latch a drained stream-error message so `status` stays diagnosable
    /// (`playback.rs:4111`).
    LatchError(String),
    /// Emit an outbound QConnect renderer-state report on a transition or the
    /// ~2 s periodic cadence (`playback.rs:4648`).
    ReportEdge,
    /// The current track ended and nothing is playable — stop
    /// (`playback.rs:4751`).
    QueueFinished,
}

/// Whether an action may run under the current authority. Playback/queue
/// mutation, owner-side resolution and session persistence are fenced while a
/// delegated renderer owns playback; diagnostics and renderer reports remain
/// live so the guest session stays observable.
fn action_allowed(action: &DriverAction, owner_actions_allowed: bool) -> bool {
    owner_actions_allowed
        || matches!(
            action,
            DriverAction::LatchError(_) | DriverAction::ReportEdge
        )
}

/// The previous tick's snapshot: the desktop loop's `last_track_id` /
/// `seen_position` / `was_playing` (plus duration for [`DriverState::after`]).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LastTick {
    pub track_id: u64,
    pub position: u64,
    pub duration: u64,
    pub is_playing: bool,
    pub engine_empty_generation: u64,
    pub source_failure_generation: u64,
}

impl LastTick {
    fn from_event(ev: &PlaybackEvent) -> Self {
        Self {
            track_id: ev.track_id,
            position: ev.position,
            duration: ev.duration,
            is_playing: ev.is_playing,
            engine_empty_generation: ev.engine_empty_generation,
            source_failure_generation: ev.source_failure_generation,
        }
    }
}

fn report_track_id(ev: &PlaybackEvent) -> u64 {
    if !matches!(ev.buffer_state, PlaybackBufferState::Idle) && ev.buffer_track_id != 0 {
        ev.buffer_track_id
    } else {
        ev.track_id
    }
}

/// The driver's carried-over state between ticks — the loop-local `mut`
/// variables of `start_poll_loop`, hoisted into a value so the decision is a
/// pure function of it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DriverState {
    /// Previous tick (`last_track_id`, `seen_position`, `was_playing`).
    pub last: LastTick,
    /// ~11-tick throttle counter for the periodic position save.
    pub save_pos_tick: u64,
    /// Track id an `ArmGapless` already fired for, so the ticker does not
    /// re-request it every tick (`gapless_requested_for`).
    pub gapless_requested_for: u64,
    /// Track id a `WarmSuccessor` already fired for (once per current track).
    pub warmed_for: u64,
    /// ~4-tick throttle counter for the periodic QConnect report.
    pub report_tick: u64,
    /// Last track id we emitted a `ReportEdge` for (`last_reported_track_id`).
    pub last_reported_track_id: u64,
    /// Last play-state we emitted a `ReportEdge` for (`last_reported_playing`).
    pub last_reported_playing: bool,
    /// Last player-owned buffer state emitted to QConnect.
    pub last_reported_buffer_state: PlaybackBufferState,
}

impl DriverState {
    /// A state whose `last` snapshot (and report trackers) come from `ev` — the
    /// "the previous tick looked like this" constructor used by the tests and by
    /// the shell to seed a baseline.
    pub fn after(ev: &PlaybackEvent) -> DriverState {
        DriverState {
            last: LastTick::from_event(ev),
            save_pos_tick: 0,
            gapless_requested_for: 0,
            warmed_for: 0,
            report_tick: 0,
            last_reported_track_id: report_track_id(ev),
            last_reported_playing: ev.is_playing,
            last_reported_buffer_state: ev.buffer_state,
        }
    }
}

/// The queue shape the decision needs, projected from `QbzCore::get_queue_state`
/// each tick: the current track id, the `(id, streamable)` upcoming list, the
/// repeat key and the stop-after marker.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct QueueSnapshot {
    pub current: u64,
    pub upcoming: Vec<(u64, bool)>,
    pub repeat: String,
    pub stop_after: Option<u64>,
    /// True when autoplay mode is "infinite" — logged-unsupported in P0 and
    /// treated as queue-finished (01 §3.1-5c).
    pub autoplay_infinite: bool,
}

/// Bounded skip-walk to the first streamable upcoming track. Returns
/// `(index, track_id)` of the first `playable == true` entry within `max_walk`
/// steps, or `None` when none is found inside the bound (never walks forever).
/// Mirrors `playback.rs::advance_to_playable`'s `MAX_OFFLINE_SKIPS` cap.
pub fn next_playable(upcoming: &[(u64, bool)], max_walk: usize) -> Option<(usize, u64)> {
    for (i, &(id, playable)) in upcoming.iter().enumerate() {
        if i >= max_walk {
            break;
        }
        if playable {
            return Some((i, id));
        }
    }
    None
}

/// The pure per-tick decision. Side-effect-free: given the carried state, the
/// live player event, the queue projection and any drained stream error, decide
/// which [`DriverAction`]s the shell must perform. Order mirrors the desktop
/// loop so the shell executes effects in the same sequence.
pub fn plan_tick(
    state: &DriverState,
    ev: &PlaybackEvent,
    queue: &QueueSnapshot,
    stream_error: Option<&str>,
) -> Vec<DriverAction> {
    let mut actions = Vec::new();
    let last = &state.last;

    // 1. Stream-error latch (playback.rs:4111): the player records a
    //    user-readable message drained exactly once per failure.
    if let Some(msg) = stream_error {
        actions.push(DriverAction::LatchError(msg.to_string()));
    }

    // 2. Periodic session-position save (playback.rs:4305-4308): ~11 ticks ≈ 5 s
    //    while a track is actually playing.
    let next_save_tick = state.save_pos_tick.wrapping_add(1);
    if ev.is_playing && ev.track_id != 0 && next_save_tick % SAVE_POSITION_EVERY_N_TICKS == 0 {
        actions.push(DriverAction::SavePosition(ev.position));
    }

    // 2b. Successor warm-up: a new current track is audible — warm the next
    //     one once, regardless of which entry started it. Placed BEFORE the
    //     seamless early-return so a hand-off warms its own successor too.
    if ev.is_playing && ev.track_id != 0 && state.warmed_for != ev.track_id {
        actions.push(DriverAction::WarmSuccessor);
    }

    // 3. Seamless gapless transition (playback.rs:4324-4371): the engine advanced
    //    to a new track WITHOUT a stop (track-id change while still playing).
    //    Sync the cursor and STOP — the desktop `continue`s before every block
    //    below, so no other action fires this tick.
    let seamless_change = ev.track_id != 0
        && last.track_id != 0
        && ev.track_id != last.track_id
        && ev.is_playing
        && last.is_playing;
    let missed_gapless_edge = ev.is_playing
        && last.track_id != ev.track_id
        && ev.track_id != 0
        && queue.current != ev.track_id
        && !matches!(ev.buffer_state, PlaybackBufferState::InitialBuffering)
        && queue
            .upcoming
            .iter()
            .find(|(_, playable)| *playable)
            .map(|(id, _)| *id)
            == Some(ev.track_id);
    if seamless_change || missed_gapless_edge {
        actions.push(DriverAction::SyncCursorTo(ev.track_id));
        return actions;
    }

    // 4. Gapless prefetch trigger (playback.rs:4387-4401): the engine wants the
    //    next track pre-queued and none is armed; arm the first playable upcoming
    //    exactly once per current track, suppressed when the current track is
    //    stop-after-marked (so it ends naturally and the marker can fire).
    if ev.is_playing
        && ev.gapless_ready
        && ev.gapless_next_track_id == 0
        && ev.track_id != 0
        && state.gapless_requested_for != ev.track_id
        && queue.stop_after != Some(ev.track_id)
    {
        if let Some(&(next_id, playable)) = queue.upcoming.first() {
            if next_id != ev.track_id && playable {
                actions.push(DriverAction::ArmGapless(next_id));
            }
        }
    }

    // 5. End-of-track/recovery edge. The engine and CMAF feeder now publish
    //    durable generations because a complete playing -> stopped pulse can
    //    fit between two 450 ms ticks. Keep the sampled predicate as a fallback
    //    for non-CMAF sources, but never require it for an explicit edge.
    let engine_empty = ev.engine_empty_generation != last.engine_empty_generation
        && ev.engine_empty_track_id != 0
        && ev.engine_empty_track_id == queue.current
        && !ev.is_playing
        && ev.gapless_next_track_id == 0;
    let source_failed = ev.source_failure_generation != last.source_failure_generation
        && ev.source_failure_track_id != 0
        && ev.source_failure_track_id == queue.current;
    let sampled_track_end = last.is_playing
        && !ev.is_playing
        && last.track_id != 0
        && (ev.track_id == 0 || ev.track_id == last.track_id)
        && ev.duration > 0
        && last.position + 2 >= ev.duration;
    let track_ended = source_failed || engine_empty || sampled_track_end;

    // 6. QConnect report edge (playback.rs:4648-4673): report on a track/play
    //    transition OR the ~2 s periodic cadence while playing. Runs regardless
    //    of `track_ended` (the desktop report block precedes the advance block).
    let next_report_tick = state.report_tick.wrapping_add(1);
    let report_track_id = report_track_id(ev);
    if report_track_id != 0 {
        let transition = report_track_id != state.last_reported_track_id
            || ev.is_playing != state.last_reported_playing
            || ev.buffer_state != state.last_reported_buffer_state;
        let buffer_active = matches!(
            ev.buffer_state,
            PlaybackBufferState::InitialBuffering | PlaybackBufferState::Underrun
        );
        let periodic = (ev.is_playing || buffer_active)
            && next_report_tick % QCONNECT_REPORT_EVERY_N_TICKS == 0;
        if transition || periodic {
            actions.push(DriverAction::ReportEdge);
        }
    }

    // 7. Track-end handling (playback.rs:4705-4762): stop-after HALTS (pause,
    //    never advance, ahead of any repeat/shuffle); else advance to the next
    //    playable track; else the queue is finished. repeat one/all always yield
    //    a next track (QueueManager owns the replay/wrap), so they never finish.
    if track_ended {
        if queue.current != 0 && queue.stop_after == Some(queue.current) {
            actions.push(DriverAction::PauseStopAfter);
        } else if queue.repeat == "one" || queue.repeat == "all" {
            actions.push(DriverAction::AdvanceAndPlay);
        } else if next_playable(&queue.upcoming, MAX_OFFLINE_SKIPS).is_some() {
            actions.push(DriverAction::AdvanceAndPlay);
        } else {
            actions.push(DriverAction::QueueFinished);
        }
    }

    actions
}

/// The pure state-update rule applied after [`plan_tick`] each tick. Replicates
/// how the desktop loop mutates its carried variables, branch by branch.
pub fn advance_state(
    prev: &DriverState,
    ev: &PlaybackEvent,
    actions: &[DriverAction],
) -> DriverState {
    let seamless = actions
        .iter()
        .any(|a| matches!(a, DriverAction::SyncCursorTo(_)));
    let armed = actions
        .iter()
        .any(|a| matches!(a, DriverAction::ArmGapless(_)));
    let ended = actions.iter().any(|a| {
        matches!(
            a,
            DriverAction::AdvanceAndPlay
                | DriverAction::PauseStopAfter
                | DriverAction::QueueFinished
        )
    });
    let reported = actions.iter().any(|a| matches!(a, DriverAction::ReportEdge));
    let warmed = actions
        .iter()
        .any(|a| matches!(a, DriverAction::WarmSuccessor));
    let warmed_for = if warmed { ev.track_id } else { prev.warmed_for };

    // save_pos_tick advances every tick — playback.rs:4305 runs before the
    // seamless `continue`.
    let save_pos_tick = prev.save_pos_tick.wrapping_add(1);

    if seamless {
        // Seamless branch (playback.rs:4363-4369): last <- ev, gapless guard
        // cleared, report trackers untouched (the `continue` precedes the report
        // block, so report_tick does NOT advance this tick).
        return DriverState {
            last: LastTick::from_event(ev),
            save_pos_tick,
            gapless_requested_for: 0,
            warmed_for,
            report_tick: prev.report_tick,
            last_reported_track_id: prev.last_reported_track_id,
            last_reported_playing: prev.last_reported_playing,
            last_reported_buffer_state: prev.last_reported_buffer_state,
        };
    }

    // Non-seamless: the report block runs (playback.rs:4648) so report_tick
    // advances; the report trackers move only when a ReportEdge fired.
    let report_tick = prev.report_tick.wrapping_add(1);
    let report_track_id = report_track_id(ev);
    let (last_reported_track_id, last_reported_playing, last_reported_buffer_state) = if reported {
        (report_track_id, ev.is_playing, ev.buffer_state)
    } else {
        (
            prev.last_reported_track_id,
            prev.last_reported_playing,
            prev.last_reported_buffer_state,
        )
    };

    // Edge trackers (playback.rs:4676-4700): last_track_id/seen_position update
    // only when track_id != 0; was_playing tracks is_playing unconditionally.
    let mut last = if ev.track_id != 0 {
        LastTick::from_event(ev)
    } else {
        LastTick {
            track_id: prev.last.track_id,
            position: prev.last.position,
            duration: prev.last.duration,
            is_playing: ev.is_playing,
            engine_empty_generation: ev.engine_empty_generation,
            source_failure_generation: ev.source_failure_generation,
        }
    };
    let mut gapless_requested_for = if armed {
        ev.track_id
    } else {
        prev.gapless_requested_for
    };

    // Track-end handler resets the edge trackers + the gapless guard
    // (playback.rs:4728-4742, both the stop-after and advance branches).
    if ended {
        // Keep the durable player generations consumed while resetting the
        // sampled edge fields. Resetting the generations to zero would replay
        // the same engine-empty/source-failure edge on every tick until the
        // next track surfaced.
        last = LastTick {
            engine_empty_generation: ev.engine_empty_generation,
            source_failure_generation: ev.source_failure_generation,
            ..LastTick::default()
        };
        gapless_requested_for = 0;
    }

    DriverState {
        last,
        save_pos_tick,
        gapless_requested_for,
        warmed_for,
        report_tick,
        last_reported_track_id,
        last_reported_playing,
        last_reported_buffer_state,
    }
}

/// Keep delegated observations useful for reports without ever promoting them
/// into the owner's carried playback state. The delegated state is discarded
/// on the first owner-admitted tick; restore then plans only from the last
/// owner snapshot captured before handoff.
fn advance_authority_state(
    owner_state: &mut DriverState,
    delegated_state: &mut Option<DriverState>,
    ev: &PlaybackEvent,
    actions: &[DriverAction],
    owner_actions_allowed: bool,
) {
    if owner_actions_allowed {
        *owner_state = advance_state(owner_state, ev, actions);
        *delegated_state = None;
    } else {
        let previous = delegated_state.take().unwrap_or_default();
        *delegated_state = Some(advance_state(&previous, ev, actions));
    }
}

/// Map the desktop `ui_prefs.streaming_quality` key to a request-layer
/// [`Quality`]. Byte-identical contract to `crates/qbz/src/ui_prefs.rs:823`
/// (`streaming_quality_for_key`), replicated here because the desktop crate is
/// out of the daemon's dependency graph. Unknown/unset keys fall back to the
/// top tier so hi-res never silently downgrades (01 §3.1).
pub fn quality_from_key(key: &str) -> Quality {
    match key {
        "mp3" => Quality::Mp3,
        "cd" => Quality::Lossless,
        "hires" => Quality::HiRes,
        _ => Quality::UltraHiRes, // "hires_plus" + unknown keys
    }
}

// ─────────────────────────── IO shell ───────────────────────────

/// Host-supplied side channels the shell drives on each relevant action. Kept
/// as trait objects so qbzd can wire daemon-shared latching / tick timestamping
/// / the QConnect report signal without this module depending on qbzd.
#[derive(Clone)]
pub struct DriverDeps {
    /// Resolve the streaming quality at play time (qbzd passes the daemon prefs).
    pub quality: Arc<dyn Fn() -> Quality + Send + Sync>,
    /// True only while the daemon's owner authority may mutate/resolve/persist.
    /// Delegated playback keeps the driver alive for diagnostics and reports,
    /// but fences every owner-side action.
    pub owner_actions_allowed: Arc<dyn Fn() -> bool + Send + Sync>,
    /// Atomically classify authority before reading player/queue state. An
    /// owner result carries the exact generation and a drain-visible permit;
    /// delegated ticks remain report-only, while fenced ticks retry unread.
    pub observe_authority: Arc<dyn Fn() -> DriverAuthorityObservation + Send + Sync>,
    /// Re-admit a spawned continuation only under the exact owner generation
    /// that produced it. A guest round-trip must never promote stale work into
    /// a later owner merely because owner actions are allowed again.
    pub readmit_owner_action:
        Arc<dyn Fn(&DriverOwnerToken) -> Option<DriverActionPermit> + Send + Sync>,
    /// Report-edge signal (T10 wires the QConnect renderer report).
    pub on_edge: Arc<dyn Fn() + Send + Sync>,
    /// Latch a drained error under a category ("stream" | "transport" | "auth").
    pub on_latch: Arc<dyn Fn(&str, String) + Send + Sync>,
    /// Called at the end of every tick (qbzd timestamps `driver_last_tick`).
    pub on_tick: Arc<dyn Fn() + Send + Sync>,
}

/// Type-erased RAII permit supplied by the host's authority coordinator.
/// `qbz-app` needs only the drop boundary; it deliberately knows nothing about
/// the daemon's concrete authority stamps.
pub struct DriverActionPermit {
    _inner: Box<dyn Send + Sync>,
}

/// Type-erased exact owner token supplied by the host authority coordinator.
/// The concrete token remains opaque to `qbz-app`; only the host callback can
/// downcast and validate it.
#[derive(Clone)]
pub struct DriverOwnerToken(Arc<dyn Any + Send + Sync>);

impl DriverOwnerToken {
    pub fn new<T: Send + Sync + 'static>(token: T) -> Self {
        Self(Arc::new(token))
    }

    pub fn downcast_ref<T: Send + Sync + 'static>(&self) -> Option<&T> {
        self.0.downcast_ref::<T>()
    }
}

impl fmt::Debug for DriverOwnerToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("DriverOwnerToken(..)")
    }
}

/// One exact owner observation plus the live permit covering the tick input.
pub struct DriverOwnerAdmission {
    token: DriverOwnerToken,
    _permit: DriverActionPermit,
}

impl DriverOwnerAdmission {
    pub fn new(token: DriverOwnerToken, permit: DriverActionPermit) -> Self {
        Self {
            token,
            _permit: permit,
        }
    }

    fn token(&self) -> DriverOwnerToken {
        self.token.clone()
    }
}

/// Host classification captured before a driver tick reads player or queue.
/// A lifecycle fence is distinct from delegated playback: fenced ticks retry
/// without reading or altering either authority's carried state.
pub enum DriverAuthorityObservation {
    Owner(DriverOwnerAdmission),
    Delegated,
    Fenced,
}

impl DriverActionPermit {
    pub fn new<T: Send + Sync + 'static>(permit: T) -> Self {
        Self {
            _inner: Box::new(permit),
        }
    }
}

/// Result of a gapless byte fetch that ran outside the 450 ms control loop.
/// Keeping the predecessor alongside the successor lets the loop reject a
/// late download after a manual track/queue change instead of appending it to
/// whichever engine happens to be current by then.
struct GaplessFetchResult {
    predecessor_id: u64,
    successor_id: u64,
    bytes: Option<Vec<u8>>,
    /// The successor was appended as an incremental CMAF stream by the
    /// player itself (`queue_gapless_streaming`); nothing left to hand over.
    streamed: bool,
}

/// A completed gapless fetch is usable only while both playback and the queue
/// still describe the edge it was started for. `is_playing` is deliberately
/// not required: `PlayNext` can safely land just after natural end and revive
/// the now-empty engine, closing the final download-vs-end race.
fn gapless_fetch_is_current(
    result: &GaplessFetchResult,
    event: &PlaybackEvent,
    queue: &QueueSnapshot,
) -> bool {
    event.track_id == result.predecessor_id
        && event.gapless_next_track_id == 0
        && queue.current == result.predecessor_id
        && queue.stop_after != Some(result.predecessor_id)
        && queue.upcoming.first().copied() == Some((result.successor_id, true))
}

async fn finish_gapless_fetch<A: FrontendAdapter + Send + Sync + 'static>(
    runtime: &Arc<AppRuntime<A>>,
    result: GaplessFetchResult,
) {
    if result.streamed {
        log::info!(
            "[qbzd] driver: successor {} queued as a gapless stream after {}",
            result.successor_id,
            result.predecessor_id
        );
        return;
    }
    if result.bytes.is_none() {
        log::warn!(
            "[qbzd] driver: gapless fetch for track {} failed",
            result.successor_id
        );
        return;
    }

    let core = runtime.core();
    let queue = queue_snapshot(core).await;
    let player = core.player();
    let event = player.get_playback_event();
    if !player.state.has_loaded_audio() || !gapless_fetch_is_current(&result, &event, &queue) {
        log::info!(
            "[qbzd] driver: discarding stale gapless track {} (predecessor was {})",
            result.successor_id,
            result.predecessor_id
        );
        return;
    }

    let bytes = result.bytes.expect("gapless bytes checked above");
    match player.play_next(bytes, result.successor_id) {
        Ok(()) => log::info!(
            "[qbzd] driver: queued track {} for gapless playback",
            result.successor_id
        ),
        Err(error) => log::warn!(
            "[qbzd] driver: gapless play_next for track {} failed: {error}",
            result.successor_id
        ),
    }
}

/// The 450 ms IO shell. Each tick: read the player event, drain the stream-error
/// latch, project the queue, `plan_tick`, execute the actions, then
/// `advance_state`. Breaks when `shutdown` flips to `true`; the loop is thin by
/// design (01 §3.2 — too thin to hide bugs). Runs safely from boot regardless of
/// auth: with no session the queue is empty and every tick is a near-no-op.
pub async fn run_driver<A: FrontendAdapter + Send + Sync + 'static>(
    runtime: Arc<AppRuntime<A>>,
    deps: DriverDeps,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    let mut state = DriverState::default();
    let mut delegated_state: Option<DriverState> = None;
    let mut ticker = tokio::time::interval(Duration::from_millis(TICK_MS));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // Gapless downloads may outlive the fixed ten-second arm window, but they
    // must never block the loop that detects natural end and advances the
    // queue. JoinSet keeps those tasks owned by the driver so shutdown can
    // abort and drain them before AppRuntime is dropped (#521 ordering).
    let mut gapless_fetches = tokio::task::JoinSet::new();
    // Successor warm-ups (cache prefetch) are best-effort downloads owned by
    // the driver for the same shutdown reason; they never block a tick.
    let mut warm_tasks: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();

    loop {
        tokio::select! {
            biased;
            changed = shutdown.changed() => {
                if changed.is_err() || *shutdown.borrow() {
                    break;
                }
                continue;
            }
            completed = gapless_fetches.join_next(), if !gapless_fetches.is_empty() => {
                match completed {
                    Some(Ok((owner_token, result))) => {
                        if let Some(_permit) = (deps.readmit_owner_action)(&owner_token) {
                            finish_gapless_fetch(&runtime, result).await;
                        } else {
                            log::debug!("[qbzd] driver: discarded gapless result from a stale owner authority");
                        }
                    }
                    Some(Err(error)) if error.is_cancelled() => {
                        log::debug!("[qbzd] driver: stale gapless fetch cancelled");
                    }
                    Some(Err(error)) => {
                        log::warn!("[qbzd] driver: gapless fetch task failed: {error}");
                    }
                    None => {}
                }
                continue;
            }
            reaped = warm_tasks.join_next(), if !warm_tasks.is_empty() => {
                if let Some(Err(error)) = reaped {
                    if !error.is_cancelled() {
                        log::warn!("[qbzd] driver: successor warm-up task failed: {error}");
                    }
                }
                continue;
            }
            _ = ticker.tick() => {}
        }

        // Capture owner admission BEFORE the first player/queue read. If this
        // tick begins under guest authority it remains guest-only even when an
        // owner restore completes during `queue_snapshot().await`.
        let owner_admission = match (deps.observe_authority)() {
            DriverAuthorityObservation::Owner(admission) => Some(admission),
            DriverAuthorityObservation::Delegated => None,
            DriverAuthorityObservation::Fenced => {
                // The transition fence is waiting for every exact permit held
                // by these network jobs. Abort them on the first fenced tick,
                // not only after delegated authority is already installed, or
                // a slow prefetch could stretch handoff to the stream timeout.
                gapless_fetches.abort_all();
                warm_tasks.abort_all();
                continue;
            }
        };
        let owner_token = owner_admission
            .as_ref()
            .map(DriverOwnerAdmission::token);
        let core = runtime.core();
        let player = core.player();
        let ev = player.get_playback_event();
        // Drain-once stream-error message (playback.rs:4111).
        let stream_error = player.state.take_stream_error_message();
        let queue = queue_snapshot(core).await;

        let owner_actions_allowed = owner_admission.is_some();
        if owner_actions_allowed && delegated_state.is_some() {
            // Rebase on the first fully owner-admitted observation after a
            // delegated span. Durable player generations may have advanced
            // while the guest was active; comparing those guest edges with the
            // pre-handoff owner snapshot could synthesize an owner auto-advance.
            state = DriverState::after(&ev);
            delegated_state = None;
        }
        let delegated_baseline = DriverState::default();
        let planning_state = if owner_actions_allowed {
            &state
        } else {
            delegated_state.as_ref().unwrap_or(&delegated_baseline)
        };
        let actions = plan_tick(planning_state, &ev, &queue, stream_error.as_deref());

        // Authority can change while an owner prefetch is in flight. Cancel
        // those best-effort jobs at the first observed delegated tick; their
        // completion paths are fenced independently above.
        if !owner_actions_allowed {
            gapless_fetches.abort_all();
            warm_tasks.abort_all();
        }

        // Once the engine/queue edge has moved, an outstanding fetch belongs
        // to the old predecessor. Drop its network future immediately so it
        // cannot contend with the normal next-track start.
        if actions.iter().any(|action| {
            matches!(
                action,
                DriverAction::SyncCursorTo(_)
                    | DriverAction::AdvanceAndPlay
                    | DriverAction::PauseStopAfter
                    | DriverAction::QueueFinished
            )
        }) {
            gapless_fetches.abort_all();
        }

        let mut executed_actions = Vec::with_capacity(actions.len());
        for action in &actions {
            if !action_allowed(action, owner_actions_allowed) {
                continue;
            }
            executed_actions.push(action.clone());
            match action {
                DriverAction::SyncCursorTo(id) => {
                    core.sync_gapless_successor(*id).await;
                }
                DriverAction::ArmGapless(id) => {
                    let quality = (deps.quality)();
                    let predecessor_id = ev.track_id;
                    let successor_id = *id;
                    let task_runtime = Arc::clone(&runtime);
                    let task_owner_actions_allowed = Arc::clone(&deps.owner_actions_allowed);
                    let task_readmit_owner_action = Arc::clone(&deps.readmit_owner_action);
                    let task_owner_token = owner_token
                        .as_ref()
                        .expect("owner action admitted without an owner token")
                        .clone();
                    log::info!(
                        "[qbzd] driver: fetching gapless track {successor_id} after {predecessor_id}"
                    );
                    gapless_fetches.spawn(async move {
                        let Some(_permit) = task_readmit_owner_action(&task_owner_token) else {
                            return (
                                task_owner_token,
                                GaplessFetchResult {
                                    predecessor_id,
                                    successor_id,
                                    bytes: None,
                                    streamed: false,
                                },
                            );
                        };
                        let core = task_runtime.core();
                        // L2 successors keep their open file; cold successors
                        // use bounded disk spools when they exceed the RAM budget.
                        // A failed setup must not fall back to a full-file Vec.
                        if task_owner_actions_allowed() {
                            match core.queue_gapless_streaming(successor_id, quality).await {
                                Ok(()) => return (
                                    task_owner_token,
                                    GaplessFetchResult { predecessor_id, successor_id, bytes: None, streamed: true },
                                ),
                                Err(error) => log::warn!(
                                    "[qbzd] driver: gapless setup for {successor_id} failed: {error}; normal advance remains available"
                                ),
                            }
                        }
                        let bytes = None;
                        (
                            task_owner_token,
                            GaplessFetchResult {
                                predecessor_id,
                                successor_id,
                                bytes,
                                streamed: false,
                            },
                        )
                    });
                }
                DriverAction::WarmSuccessor => {
                    let quality = (deps.quality)();
                    let task_runtime = Arc::clone(&runtime);
                    let task_owner_actions_allowed = Arc::clone(&deps.owner_actions_allowed);
                    let task_readmit_owner_action = Arc::clone(&deps.readmit_owner_action);
                    let task_owner_token = owner_token
                        .as_ref()
                        .expect("owner action admitted without an owner token")
                        .clone();
                    warm_tasks.spawn(async move {
                        let Some(_permit) = task_readmit_owner_action(&task_owner_token) else {
                            return;
                        };
                        prefetch_successors(
                            task_runtime.as_ref(),
                            quality,
                            task_owner_actions_allowed.as_ref(),
                        )
                        .await;
                    });
                }
                DriverAction::PauseStopAfter => {
                    // The ended track is the queue's current track (playback.rs:4708).
                    let finished = queue.current;
                    if finished != 0 && core.consume_stop_after_if(finished).await {
                        if let Err(e) = core.pause() {
                            log::warn!("[qbzd] driver: stop-after pause failed: {e}");
                        }
                    } else {
                        // Marker cleared between the snapshot and the consume — fall
                        // through to the normal advance (desktop parity: stop-after
                        // and advance share one track-end block).
                        advance_and_play_logged(&runtime, (deps.quality)()).await;
                    }
                }
                DriverAction::AdvanceAndPlay => {
                    advance_and_play_logged(&runtime, (deps.quality)()).await;
                }
                DriverAction::SavePosition(p) => {
                    runtime.with_session_store(|s| {
                        if let Err(e) = s.save_position(*p) {
                            log::debug!("[qbzd] driver: save_position failed: {e}");
                        }
                    });
                }
                DriverAction::LatchError(m) => {
                    (deps.on_latch)("stream", m.clone());
                }
                DriverAction::ReportEdge => {
                    (deps.on_edge)();
                }
                DriverAction::QueueFinished => {
                    if queue.autoplay_infinite {
                        log::info!(
                            "[qbzd] driver: autoplay 'infinite' unsupported on qbzd v1, \
                             treated as queue-finished"
                        );
                    }
                    log::info!("[qbzd] driver: queue finished");
                    if let Err(e) = core.stop() {
                        log::warn!("[qbzd] driver: stop on queue-finished failed: {e}");
                    }
                }
            }
        }

        advance_authority_state(
            &mut state,
            &mut delegated_state,
            &ev,
            &executed_actions,
            owner_actions_allowed,
        );
        (deps.on_tick)();
    }
    warm_tasks.abort_all();
    while warm_tasks.join_next().await.is_some() {}
    gapless_fetches.abort_all();
    while let Some(result) = gapless_fetches.join_next().await {
        if let Err(error) = result {
            if !error.is_cancelled() {
                log::warn!("[qbzd] driver: gapless fetch failed during shutdown: {error}");
            }
        }
    }
    log::info!("[qbzd] driver: shutting down");
}

/// Run the advance ritual and log its outcome (queue-finished on `Ok(None)`, the
/// error on `Err`). The daemon stops on a genuine queue edge just like the
/// pure-decision `QueueFinished` branch does. Always forward — the driver's
/// auto-advance on track-end never walks backward (reverse is the `qbzd prev`
/// route's concern, wired directly through `advance_and_play(..., false)`).
async fn advance_and_play_logged<A: FrontendAdapter + Send + Sync + 'static>(
    runtime: &Arc<AppRuntime<A>>,
    quality: Quality,
) {
    match advance_and_play(runtime, quality, true).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            log::info!("[qbzd] driver: advance found nothing playable — queue finished");
            if let Err(e) = runtime.core().stop() {
                log::warn!("[qbzd] driver: stop after empty advance failed: {e}");
            }
        }
        Err(e) => log::warn!("[qbzd] driver: advance failed: {e}"),
    }
}

/// The FULL advance ritual, reused verbatim by the CLI next/prev routes (T7):
/// bounded skip-walk to the next (or previous) playable track → play it → warm
/// the successors for gapless → persist the session. Never a bare cursor move
/// (02 §2.2). The skip-walk mirrors `playback.rs::advance_to_playable` (capped
/// at `MAX_OFFLINE_SKIPS`); `next_track()`/`previous_track()` are the atomic
/// cursor movers — `forward` selects which one, exactly like the desktop's
/// `advance_to_playable(runtime, weak, forward)` (`crates/qbz/src/playback.rs:358`),
/// so `qbzd next` and `qbzd prev` share this one ritual instead of duplicating
/// the play → prefetch → persist tail.
pub async fn advance_and_play<A: FrontendAdapter + Send + Sync + 'static>(
    runtime: &AppRuntime<A>,
    quality: Quality,
    forward: bool,
) -> Result<Option<QueueTrack>, String> {
    let core = runtime.core();
    let mut skips = 0usize;
    let next = loop {
        let step = if forward {
            core.next_track().await
        } else {
            core.previous_track().await
        };
        let Some(track) = step else {
            break None; // queue edge
        };
        // Playable gate: local files always attempt (they play from disk);
        // streamable Qobuz tracks OK. Daemon P0 has no offline tier, so an
        // unstreamable remote track is a skip (mirrors advance_to_playable).
        if track.streamable || track.is_local {
            break Some(track);
        }
        skips += 1;
        log::info!("[qbzd] driver: skipping unavailable track {} ({skips}/{MAX_OFFLINE_SKIPS})", track.id);
        if skips >= MAX_OFFLINE_SKIPS {
            let _ = core.stop();
            break None;
        }
    };
    let Some(track) = next else {
        return Ok(None);
    };
    let track_id = track.id;
    core.play_track_resolved(track_id, quality, None, None, 0)
        .await?;
    // The successor warm-up is the driver's `WarmSuccessor` action (fired on
    // the first tick that sees the new track, for EVERY entry point) — never
    // awaited here, where a full download used to stall the control loop.
    // Persist the session (queue + current + position) so a restart resumes.
    save_session_now(runtime).await;
    Ok(Some(track))
}

/// Warm the player cache for the next upcoming track (best-effort; failures are
/// logged, never fatal). Mirrors `playback.rs::kick_prefetch` for the daemon:
/// only remote, non-local, not-already-cached tracks are fetched.
async fn prefetch_successors<A: FrontendAdapter + Send + Sync + 'static>(
    runtime: &AppRuntime<A>,
    quality: Quality,
    owner_actions_allowed: &(dyn Fn() -> bool + Send + Sync),
) {
    if !owner_actions_allowed() {
        return;
    }
    // Memory-pressure watchdog: critical pressure latched a halt — do not
    // start a new download (issue #660).
    if crate::memory_watchdog::prefetch_halted() {
        log::debug!("[qbzd] driver: prefetch skipped — memory-pressure halt active");
        return;
    }
    let core = runtime.core();
    let profile = core.player().playback_prefetch_policy();
    // Depth honors the profile's prefetch_count cap. The daemon's
    // historical depth is 1 successor per advance, which is within BOTH
    // classes' caps (LowMemory 1, Normal 5), so this changes nothing today
    // and clamps automatically if the depth ever grows. There is no
    // concurrency loop here — the single prefetch below is awaited — so
    // max_concurrent_prefetch (>= 1 in both classes) is trivially honored.
    let depth = 1usize.min(profile.lookahead);
    let upcoming = core.peek_upcoming(depth).await;
    let Some(next) = upcoming.into_iter().next() else {
        return;
    };
    if next.is_local {
        return;
    }
    let player = core.player();
    if player.is_track_cached(next.id) {
        return;
    }
    if !owner_actions_allowed() {
        return;
    }
    let client_lock = core.client();
    let guard = client_lock.read().await;
    let Some(client) = guard.as_ref() else {
        return;
    };
    // Quality cap: the LowMemory profile disallows HiRes prefetch entirely
    // (~60 MB/track → ~15 MB at Lossless); the watchdog additionally pauses
    // HiRes prefetch on low-pressure ticks. This caps only the warm-ahead
    // copy — playback of the user's selected quality is unaffected.
    let allow_hires =
        profile.allow_hires && !crate::memory_watchdog::hires_prefetch_paused();
    let prefetch_quality = cap_prefetch_quality(quality, allow_hires);
    if prefetch_quality != quality {
        // Do not warm a CD copy that will be rejected at a Hi-Res transition.
        // The on-demand gapless spool retains the requested format.
        return;
    }
    if !owner_actions_allowed() {
        return;
    }
    if let Err(e) = player
        .prefetch_into_cache(client, next.id, prefetch_quality)
        .await
    {
        log::debug!("[qbzd] driver: prefetch track {} failed: {e}", next.id);
    }
}

/// Pure prefetch-quality cap: when HiRes prefetch is disallowed (LowMemory
/// profile or a low-pressure watchdog tick), HiRes/UltraHiRes warm-aheads
/// downgrade to Lossless; anything else passes through unchanged.
fn cap_prefetch_quality(requested: Quality, allow_hires: bool) -> Quality {
    if allow_hires {
        return requested;
    }
    match requested {
        Quality::HiRes | Quality::UltraHiRes => Quality::Lossless,
        q => q,
    }
}

/// Project the live queue into the decision's [`QueueSnapshot`].
async fn queue_snapshot<A: FrontendAdapter + Send + Sync + 'static>(
    core: &QbzCore<A>,
) -> QueueSnapshot {
    let state = core.get_queue_state_full().await;
    let current = state.current_track.as_ref().map(|t| t.id).unwrap_or(0);
    let upcoming = state
        .upcoming
        .iter()
        .map(|t| (t.id, t.streamable || t.is_local))
        .collect();
    QueueSnapshot {
        current,
        upcoming,
        repeat: repeat_to_str(state.repeat).to_string(),
        stop_after: state.stop_after_track_id,
        // Autoplay "infinite" is a later-task wiring; P0 never sets it.
        autoplay_infinite: false,
    }
}

// ───────────────────── session persistence (daemon) ─────────────────────

/// Capture the live queue + playback state and persist it via the active
/// session store. No-op when no session is active (`with_session_store` returns
/// `None`). Mirrors `crates/qbz/src/session_persist.rs::capture_and_save`, minus
/// the desktop-only `persist_session` gate (the daemon's store IS its queue
/// persistence, so it always saves).
pub async fn save_session_now<A: FrontendAdapter + Send + Sync + 'static>(runtime: &AppRuntime<A>) {
    let core = runtime.core();
    let (tracks, current_index) = core.get_all_queue_tracks().await;
    let full = core.get_queue_state_full().await;
    let ev = core.player().get_playback_event();
    let snapshot = PersistedSessionSnapshot {
        playback: PersistedPlaybackSession {
            queue_tracks: tracks.iter().map(to_persisted).collect(),
            current_index,
            current_position_secs: ev.position,
            volume: ev.volume,
            shuffle_enabled: full.shuffle,
            repeat_mode: repeat_to_str(full.repeat).to_string(),
            was_playing: ev.is_playing,
            saved_at: 0, // set inside save_session
        },
        // Shell-view columns are desktop-only; keep defaults so the schema
        // round-trips unchanged.
        shell_view: PersistedShellViewState::default(),
    };
    let saved = runtime.with_session_store(|s| s.save_session(&snapshot));
    match saved {
        Some(Ok(())) => {}
        Some(Err(e)) => log::warn!("[qbzd] driver: session save failed: {e}"),
        None => log::debug!("[qbzd] driver: session save skipped (no active session)"),
    }
}

/// Restore the persisted queue at boot, PAUSED (queue + order + repeat + volume,
/// never auto-playing). Returns `true` when a non-empty queue was restored.
/// Mirrors `session_persist::restore`'s Phase A; the daemon has no
/// `resume_playback_position` gate, so the saved position is threaded into the
/// snapshot but only replayed when the CLI later plays the restored track.
pub async fn restore_session_paused<A: FrontendAdapter + Send + Sync + 'static>(
    runtime: &AppRuntime<A>,
) -> bool {
    let Some(loaded) = runtime.with_session_store(|s| s.load_session()) else {
        return false; // no active session
    };
    let snapshot = match loaded {
        Ok(s) => s,
        Err(e) => {
            log::warn!("[qbzd] driver: session load failed: {e}");
            return false;
        }
    };
    let pb = snapshot.playback;
    if pb.queue_tracks.is_empty() {
        log::info!("[qbzd] driver: nothing to restore (saved queue is empty)");
        return false;
    }
    let count = pb.queue_tracks.len();
    let index = pb.current_index;
    let position = pb.current_position_secs;
    let tracks: Vec<QueueTrack> = pb.queue_tracks.into_iter().map(from_persisted).collect();
    let core = runtime.core();
    core.set_queue_with_order(tracks, index, pb.shuffle_enabled, None)
        .await;
    core.set_repeat_mode(repeat_from_str(&pb.repeat_mode)).await;
    let _ = core.set_volume(pb.volume);
    log::info!(
        "[qbzd] driver: restored {count} queue tracks (index {index:?}), paused; \
         saved position {position}s"
    );
    true
}

fn repeat_to_str(mode: RepeatMode) -> &'static str {
    match mode {
        RepeatMode::Off => "off",
        RepeatMode::All => "all",
        RepeatMode::One => "one",
    }
}

fn repeat_from_str(s: &str) -> RepeatMode {
    match s {
        "all" => RepeatMode::All,
        "one" => RepeatMode::One,
        _ => RepeatMode::Off,
    }
}

fn to_persisted(t: &QueueTrack) -> PersistedQueueTrack {
    PersistedQueueTrack {
        id: t.id,
        title: t.title.clone(),
        artist: t.artist.clone(),
        album: t.album.clone(),
        duration_secs: t.duration_secs,
        artwork_url: t.artwork_url.clone(),
        hires: t.hires,
        bit_depth: t.bit_depth,
        sample_rate: t.sample_rate,
        is_local: t.is_local,
        album_id: t.album_id.clone(),
        artist_id: t.artist_id,
        streamable: t.streamable,
        source: t.source.clone(),
        parental_warning: t.parental_warning,
        source_item_id_hint: t.source_item_id_hint.clone(),
    }
}

fn from_persisted(t: PersistedQueueTrack) -> QueueTrack {
    QueueTrack {
        id: t.id,
        title: t.title,
        version: None,
        artist: t.artist,
        album: t.album,
        album_version: None,
        duration_secs: t.duration_secs,
        artwork_url: t.artwork_url,
        hires: t.hires,
        bit_depth: t.bit_depth,
        sample_rate: t.sample_rate,
        is_local: t.is_local,
        album_id: t.album_id,
        artist_id: t.artist_id,
        streamable: t.streamable,
        source: t.source,
        parental_warning: t.parental_warning,
        source_item_id_hint: t.source_item_id_hint,
        context_kind: None,
        context_id: None,
        isrc: None,
        recording_mbid: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal `PlaybackEvent` builder: the four fields the driver reasons about
    /// plus Default-equivalents for the rest (gapless fields off, no stream meta).
    fn ev(track: u64, playing: bool, pos: u64, dur: u64) -> PlaybackEvent {
        PlaybackEvent {
            is_playing: playing,
            position: pos,
            duration: dur,
            track_id: track,
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
            buffer_state: PlaybackBufferState::Idle,
            buffer_track_id: 0,
            engine_empty_generation: 0,
            engine_empty_track_id: 0,
            source_failure_generation: 0,
            source_failure_track_id: 0,
        }
    }

    /// Queue-shape builder: current track id, `(id, streamable)` upcoming list,
    /// repeat key ("off"|"all"|"one"), optional stop-after marker.
    fn q(
        current: u64,
        upcoming: &[(u64, bool)],
        repeat: &str,
        stop_after: Option<u64>,
    ) -> QueueSnapshot {
        QueueSnapshot {
            current,
            upcoming: upcoming.to_vec(),
            repeat: repeat.to_string(),
            stop_after,
            autoplay_infinite: false,
        }
    }

    #[test]
    fn gapless_cursor_recovers_after_a_nonplaying_transition_tick() {
        let old = ev(1, false, 200, 200);
        let state = DriverState::after(&old);
        let live = ev(2, true, 1, 180);
        let queue = q(1, &[(2, true)], "off", None);
        assert!(plan_tick(&state, &live, &queue, None).contains(&DriverAction::SyncCursorTo(2)));
    }
    #[test]
    fn cursor_recovery_does_not_starve_reports_or_override_pending_loads() {
        let live = ev(2, true, 1, 180);
        let state = DriverState::after(&live);
        let queue = q(1, &[(2, true)], "off", None);
        assert!(!plan_tick(&state, &live, &queue, None).contains(&DriverAction::SyncCursorTo(2)));
        let state = DriverState::after(&ev(1, false, 180, 180));
        let mut loading = live.clone();
        loading.buffer_state = PlaybackBufferState::InitialBuffering;
        assert!(!plan_tick(&state, &loading, &queue, None).contains(&DriverAction::SyncCursorTo(2)));
        assert!(!plan_tick(
            &state,
            &live,
            &q(1, &[(3, true), (2, true)], "off", None),
            None
        )
        .contains(&DriverAction::SyncCursorTo(2)));
    }

    #[test]
    fn delegated_authority_fences_every_owner_action() {
        let owner_actions = [
            DriverAction::SyncCursorTo(2),
            DriverAction::ArmGapless(2),
            DriverAction::WarmSuccessor,
            DriverAction::AdvanceAndPlay,
            DriverAction::PauseStopAfter,
            DriverAction::SavePosition(17),
            DriverAction::QueueFinished,
        ];

        for action in &owner_actions {
            assert!(action_allowed(action, true), "owner rejected {action:?}");
            assert!(
                !action_allowed(action, false),
                "delegated authority accepted {action:?}"
            );
        }

        for action in [
            DriverAction::LatchError("stream failed".to_string()),
            DriverAction::ReportEdge,
        ] {
            assert!(action_allowed(&action, true));
            assert!(action_allowed(&action, false));
        }
    }

    #[test]
    fn delegated_filter_keeps_diagnostics_in_planned_order() {
        let state = DriverState::after(&ev(1, true, 580, 581));
        let planned = plan_tick(
            &state,
            &ev(1, false, 581, 581),
            &q(1, &[(2, true)], "off", None),
            Some("stream failed"),
        );
        assert!(planned.contains(&DriverAction::AdvanceAndPlay));

        let executable: Vec<_> = planned
            .into_iter()
            .filter(|action| action_allowed(action, false))
            .collect();
        assert_eq!(
            executable,
            vec![
                DriverAction::LatchError("stream failed".to_string()),
                DriverAction::ReportEdge,
            ]
        );
    }

    #[test]
    fn delegated_snapshots_never_replace_owner_driver_state() {
        let owner_event = ev(11, true, 42, 300);
        let mut owner_state = DriverState::after(&owner_event);
        let original_owner_state = owner_state.clone();
        let mut delegated_state = None;
        let guest_event = ev(99, true, 120, 240);

        advance_authority_state(
            &mut owner_state,
            &mut delegated_state,
            &guest_event,
            &[DriverAction::ReportEdge],
            false,
        );

        assert_eq!(owner_state, original_owner_state);
        assert_eq!(
            delegated_state.as_ref().map(|state| state.last.track_id),
            Some(99)
        );

        let restored_event = ev(11, true, 43, 300);
        advance_authority_state(
            &mut owner_state,
            &mut delegated_state,
            &restored_event,
            &[],
            true,
        );
        assert_eq!(owner_state.last.track_id, 11);
        assert_eq!(owner_state.last.position, 43);
        assert!(delegated_state.is_none());
    }

    #[test]
    fn end_edge_advances() {
        let s = DriverState::after(&ev(1, true, 580, 581));
        let a = plan_tick(
            &s,
            &ev(1, false, 581, 581),
            &q(1, &[(2, true)], "off", None),
            None,
        );
        assert!(a.contains(&DriverAction::AdvanceAndPlay));
        assert!(a.contains(&DriverAction::ReportEdge)); // play-state edge
    }

    #[test]
    fn engine_empty_generation_advances_when_playing_pulse_was_missed() {
        // Both sampled ticks are stopped. This is the observed 2026-08-28
        // failure: a late gapless resume and engine-empty happened inside one
        // polling interval, so `last.is_playing` never became true.
        let s = DriverState::after(&ev(1, false, 580, 581));
        let mut current = ev(1, false, 581, 581);
        current.engine_empty_generation = 1;
        current.engine_empty_track_id = 1;

        let actions = plan_tick(&s, &current, &q(1, &[(2, true)], "off", None), None);
        assert!(actions.contains(&DriverAction::AdvanceAndPlay));
    }

    #[test]
    fn source_failure_advances_even_when_track_never_started() {
        let s = DriverState::after(&ev(0, false, 0, 0));
        let mut current = ev(0, false, 0, 0);
        current.source_failure_generation = 1;
        current.source_failure_track_id = 1;

        let actions = plan_tick(
            &s,
            &current,
            &q(1, &[(2, true)], "off", None),
            Some("CMAF segment retries exhausted"),
        );
        assert!(actions.contains(&DriverAction::LatchError(
            "CMAF segment retries exhausted".to_string()
        )));
        assert!(actions.contains(&DriverAction::AdvanceAndPlay));
    }

    #[test]
    fn stale_source_failure_cannot_skip_the_new_queue_track() {
        let s = DriverState::after(&ev(2, false, 0, 581));
        let mut current = ev(2, false, 0, 581);
        current.source_failure_generation = 1;
        current.source_failure_track_id = 1;

        let actions = plan_tick(&s, &current, &q(2, &[(3, true)], "off", None), None);
        assert!(!actions.contains(&DriverAction::AdvanceAndPlay));
    }

    #[test]
    fn consumed_engine_empty_generation_does_not_advance_twice() {
        let s = DriverState::after(&ev(1, false, 580, 581));
        let mut current = ev(1, false, 581, 581);
        current.engine_empty_generation = 1;
        current.engine_empty_track_id = 1;
        let queue = q(1, &[(2, true)], "off", None);
        let first = plan_tick(&s, &current, &queue, None);
        assert!(first.contains(&DriverAction::AdvanceAndPlay));

        let advanced = advance_state(&s, &current, &first);
        let second = plan_tick(&advanced, &current, &queue, None);
        assert!(!second.contains(&DriverAction::AdvanceAndPlay));
    }

    #[test]
    fn late_gapless_rescue_consumes_engine_empty_without_advancing() {
        let s = DriverState::after(&ev(1, false, 580, 581));
        let mut current = ev(1, true, 581, 581);
        current.engine_empty_generation = 1;
        current.engine_empty_track_id = 1;
        current.gapless_next_track_id = 2;

        let queue = q(1, &[(2, true)], "off", None);
        let first = plan_tick(&s, &current, &queue, None);
        assert!(!first.contains(&DriverAction::AdvanceAndPlay));

        let advanced = advance_state(&s, &current, &first);
        let second = plan_tick(&advanced, &current, &queue, None);
        assert!(!second.contains(&DriverAction::AdvanceAndPlay));
    }

    #[test]
    fn mid_track_pause_does_not_advance() {
        let s = DriverState::after(&ev(1, true, 100, 581));
        let a = plan_tick(
            &s,
            &ev(1, false, 100, 581),
            &q(1, &[(2, true)], "off", None),
            None,
        );
        assert!(!a.contains(&DriverAction::AdvanceAndPlay));
        assert!(a.contains(&DriverAction::ReportEdge));
    }

    #[test]
    fn buffer_transition_emits_qconnect_report_edge() {
        let mut ready = ev(41, true, 100, 581);
        ready.buffer_state = PlaybackBufferState::Ready;
        ready.buffer_track_id = 41;
        let state = DriverState::after(&ready);

        let mut underrun = ready.clone();
        underrun.buffer_state = PlaybackBufferState::Underrun;
        let actions = plan_tick(&state, &underrun, &q(41, &[(42, true)], "off", None), None);

        assert!(actions.contains(&DriverAction::ReportEdge));
    }

    #[test]
    fn stop_after_one_shot() {
        let s = DriverState::after(&ev(42, true, 580, 581));
        let a = plan_tick(
            &s,
            &ev(42, false, 581, 581),
            &q(42, &[(2, true)], "off", Some(42)),
            None,
        );
        assert!(a.contains(&DriverAction::PauseStopAfter));
        assert!(!a.contains(&DriverAction::AdvanceAndPlay));
    }

    #[test]
    fn warm_successor_fires_once_per_track_for_any_entry() {
        // A track appears playing (started by API/QConnect/CLI — the driver
        // does not care which): warm once, then stay quiet for that id.
        let e = ev(1, true, 3, 581);
        let s = DriverState::after(&ev(1, true, 2, 581));
        let queue = q(1, &[(2, true)], "off", None);
        let a1 = plan_tick(&s, &e, &queue, None);
        assert!(a1.contains(&DriverAction::WarmSuccessor));
        let s2 = advance_state(&s, &e, &a1);
        assert_eq!(s2.warmed_for, 1);
        let a2 = plan_tick(&s2, &e, &queue, None);
        assert!(!a2.contains(&DriverAction::WarmSuccessor));
        // Paused: no warm for a track that is not audible yet.
        let paused = ev(5, false, 0, 581);
        let a3 = plan_tick(&s2, &paused, &q(5, &[(6, true)], "off", None), None);
        assert!(!a3.contains(&DriverAction::WarmSuccessor));
    }

    #[test]
    fn seamless_edge_warms_the_new_successor() {
        // 1 -> 2 hand-off without a stop: the cursor sync AND a warm for 2's
        // successor fire on the same tick (the #699 follow-up: every edge
        // after a seamless one used to be cold).
        let prev = ev(1, true, 580, 581);
        let mut s = DriverState::after(&prev);
        s.warmed_for = 1;
        let e = ev(2, true, 0, 300);
        let a = plan_tick(&s, &e, &q(1, &[(2, true), (3, true)], "off", None), None);
        assert!(a.contains(&DriverAction::SyncCursorTo(2)));
        assert!(a.contains(&DriverAction::WarmSuccessor));
        let s2 = advance_state(&s, &e, &a);
        assert_eq!(s2.warmed_for, 2);
        assert_eq!(s2.gapless_requested_for, 0);
    }

    #[test]
    fn gapless_arms_exactly_once() {
        let mut e = ev(1, true, 300, 581);
        e.gapless_ready = true;
        e.gapless_next_track_id = 0;
        let s = DriverState::after(&ev(1, true, 299, 581));
        let queue = q(1, &[(2, true)], "off", None);
        let a1 = plan_tick(&s, &e, &queue, None);
        assert!(a1.contains(&DriverAction::ArmGapless(2)));
        let s2 = advance_state(&s, &e, &a1);
        let a2 = plan_tick(&s2, &e, &queue, None);
        assert!(!a2.iter().any(|x| matches!(x, DriverAction::ArmGapless(_))));
    }

    #[test]
    fn stopped_track_never_starts_a_gapless_fetch() {
        let mut current = ev(1, false, 581, 581);
        current.gapless_ready = true;
        let previous = DriverState::after(&ev(1, true, 580, 581));
        let actions = plan_tick(&previous, &current, &q(1, &[(2, true)], "off", None), None);
        assert!(!actions
            .iter()
            .any(|action| matches!(action, DriverAction::ArmGapless(_))));
        assert!(actions.contains(&DriverAction::AdvanceAndPlay));
    }

    #[test]
    fn completed_gapless_fetch_requires_the_original_queue_edge() {
        let result = GaplessFetchResult {
            predecessor_id: 1,
            successor_id: 2,
            bytes: None,
            streamed: false,
        };
        let mut current = ev(1, true, 575, 581);
        current.gapless_ready = true;
        assert!(gapless_fetch_is_current(
            &result,
            &current,
            &q(1, &[(2, true)], "off", None)
        ));

        // Natural end still accepts a just-finished download: PlayNext's
        // late-arrival path revives the empty engine without replaying track 1.
        current.is_playing = false;
        assert!(gapless_fetch_is_current(
            &result,
            &current,
            &q(1, &[(2, true)], "off", None)
        ));

        current.track_id = 2;
        assert!(!gapless_fetch_is_current(
            &result,
            &current,
            &q(2, &[(3, true)], "off", None)
        ));
    }

    #[test]
    fn completed_gapless_fetch_rejects_changed_successor_and_stop_after() {
        let result = GaplessFetchResult {
            predecessor_id: 1,
            successor_id: 2,
            bytes: None,
            streamed: false,
        };
        let mut current = ev(1, true, 575, 581);
        current.gapless_ready = true;
        assert!(!gapless_fetch_is_current(
            &result,
            &current,
            &q(1, &[(3, true)], "off", None)
        ));
        assert!(!gapless_fetch_is_current(
            &result,
            &current,
            &q(1, &[(2, true)], "off", Some(1))
        ));

        current.gapless_next_track_id = 9;
        assert!(!gapless_fetch_is_current(
            &result,
            &current,
            &q(1, &[(2, true)], "off", None)
        ));
    }

    #[test]
    fn repeat_one_advances_instead_of_finishing() {
        let s = DriverState::after(&ev(1, true, 580, 581));
        let a = plan_tick(&s, &ev(1, false, 581, 581), &q(1, &[], "one", None), None);
        assert!(a.contains(&DriverAction::AdvanceAndPlay));
        assert!(!a.contains(&DriverAction::QueueFinished));
    }

    #[test]
    fn queue_finished_when_nothing_playable() {
        let s = DriverState::after(&ev(1, true, 580, 581));
        let a = plan_tick(
            &s,
            &ev(1, false, 581, 581),
            &q(1, &[(2, false)], "off", None),
            None,
        );
        assert!(a.contains(&DriverAction::QueueFinished));
    }

    #[test]
    fn skip_walk_bounds() {
        assert_eq!(
            next_playable(&[(2, false), (3, false), (4, true)], 50),
            Some((2, 4))
        );
        let all_bad: Vec<(u64, bool)> = (0..60).map(|i| (i, false)).collect();
        assert_eq!(next_playable(&all_bad, 50), None); // bounded — never walks forever
    }

    #[test]
    fn position_save_cadence_11_ticks() {
        let mut s = DriverState::after(&ev(1, true, 10, 581));
        for tick in 1..=11u32 {
            let e = ev(1, true, 10 + tick as u64, 581);
            let a = plan_tick(&s, &e, &q(1, &[], "off", None), None);
            if tick == 11 {
                assert!(a.contains(&DriverAction::SavePosition(21)));
            } else {
                assert!(!a.iter().any(|x| matches!(x, DriverAction::SavePosition(_))));
            }
            s = advance_state(&s, &e, &a);
        }
    }

    #[test]
    fn seamless_gapless_transition_syncs_cursor() {
        let s = DriverState::after(&ev(1, true, 580, 581));
        let a = plan_tick(
            &s,
            &ev(2, true, 0, 547),
            &q(1, &[(2, true)], "off", None),
            None,
        );
        assert!(a.contains(&DriverAction::SyncCursorTo(2)));
    }

    #[test]
    fn stream_error_latches() {
        let s = DriverState::after(&ev(1, true, 10, 581));
        let a = plan_tick(
            &s,
            &ev(1, false, 10, 581),
            &q(1, &[], "off", None),
            Some("ALSA device disappeared"),
        );
        assert!(a.contains(&DriverAction::LatchError("ALSA device disappeared".into())));
    }

    #[test]
    fn duration_zero_never_advances() {
        let s = DriverState::after(&ev(1, true, 580, 581));
        let a = plan_tick(
            &s,
            &ev(1, false, 580, 0),
            &q(1, &[(2, true)], "off", None),
            None,
        );
        assert!(!a.contains(&DriverAction::AdvanceAndPlay));
        assert!(a.contains(&DriverAction::ReportEdge)); // play-state edge
    }

    #[test]
    fn prefetch_quality_uncapped_when_hires_allowed() {
        // Normal profile, healthy pressure: every tier passes through.
        for q in [
            Quality::Mp3,
            Quality::Lossless,
            Quality::HiRes,
            Quality::UltraHiRes,
        ] {
            assert_eq!(cap_prefetch_quality(q, true), q);
        }
    }

    #[test]
    fn prefetch_quality_caps_hires_to_lossless_when_disallowed() {
        // LowMemory profile / low-pressure tick: HiRes warm-aheads shrink.
        assert_eq!(cap_prefetch_quality(Quality::HiRes, false), Quality::Lossless);
        assert_eq!(
            cap_prefetch_quality(Quality::UltraHiRes, false),
            Quality::Lossless
        );
        // Lower tiers are untouched.
        assert_eq!(cap_prefetch_quality(Quality::Lossless, false), Quality::Lossless);
        assert_eq!(cap_prefetch_quality(Quality::Mp3, false), Quality::Mp3);
    }
}
