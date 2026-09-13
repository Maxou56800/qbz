//! A-B loop (player bar "+" menu): repeat a section of the CURRENT track.
//!
//! Process-lifetime, never persisted. Cleared on track change, stop, remote
//! takeover (cast / QConnect) and on a manual seek outside the section. The
//! watcher is a 100 ms task that only exists while a loop is active; it reads
//! the player's millisecond position (a read-only derivation) and issues the
//! same local seek the seekbar does. The audio thread is untouched.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use qbz_app::shell::AppRuntime;
use qbz_core::LoggingAdapter;

/// B must sit at least this far after A.
const MIN_LOOP_MS: u64 = 2_000;
const WATCH_INTERVAL: Duration = Duration::from_millis(100);
/// Fire this much before B so the jump lands on B, not past it.
const LEAD_MS: u64 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Loop {
    pub track_id: u64,
    /// Whole seconds: `core.seek` is second-granular.
    pub a_secs: u64,
    /// Milliseconds: the watcher compares against the ms position.
    pub b_ms: Option<u64>,
}

static STATE: OnceLock<Mutex<Option<Loop>>> = OnceLock::new();
/// Bumped on every clear / re-arm; a watcher exits when its value differs.
static GENERATION: AtomicU64 = AtomicU64::new(0);

fn slot() -> &'static Mutex<Option<Loop>> {
    STATE.get_or_init(|| Mutex::new(None))
}

pub(crate) fn current() -> Option<Loop> {
    *slot().lock().unwrap_or_else(|p| p.into_inner())
}

/// 0 = no loop, 1 = A armed (waiting for B), 2 = looping.
fn state_code(l: Option<Loop>) -> i32 {
    match l {
        None => 0,
        Some(Loop { b_ms: None, .. }) => 1,
        Some(_) => 2,
    }
}

/// What one press of the single "mark" verb does.
fn next_after_mark(cur: Option<Loop>, track_id: u64, pos_ms: u64) -> Option<Loop> {
    match cur {
        Some(l) if l.track_id == track_id => match l.b_ms {
            None if pos_ms >= l.a_secs * 1000 + MIN_LOOP_MS => Some(Loop {
                b_ms: Some(pos_ms),
                ..l
            }),
            None => Some(l),
            Some(_) => None,
        },
        _ => Some(Loop {
            track_id,
            a_secs: pos_ms / 1000,
            b_ms: None,
        }),
    }
}

fn should_jump(l: &Loop, pos_ms: u64) -> bool {
    matches!(l.b_ms, Some(b) if pos_ms + LEAD_MS >= b)
}

/// A manual seek keeps the loop only while it lands inside the section
/// (or, with only A armed, at/after A).
fn survives_manual_seek(l: &Loop, target_secs: u64) -> bool {
    if target_secs < l.a_secs {
        return false;
    }
    match l.b_ms {
        Some(b) => target_secs * 1000 <= b,
        None => true,
    }
}

fn publish(l: Option<Loop>) {
    let state = state_code(l);
    let a = l.map(|l| l.a_secs as i32).unwrap_or(0);
    let b = l
        .and_then(|l| l.b_ms)
        .map(|b| (b / 1000) as i32)
        .unwrap_or(0);
    crate::player_bridge::ui(move |mut bridge| {
        bridge.as_mut().set_ab_state(state);
        bridge.as_mut().set_ab_start_secs(a);
        bridge.as_mut().set_ab_end_secs(b);
    });
}

fn set(l: Option<Loop>) {
    *slot().lock().unwrap_or_else(|p| p.into_inner()) = l;
    GENERATION.fetch_add(1, Ordering::SeqCst);
    publish(l);
}

pub(crate) fn clear() {
    if current().is_some() {
        log::info!("[qbz-qt] A-B loop cleared");
        set(None);
    }
}

/// The "+" menu verb: arm A, then B, then clear. Local playback only.
pub(crate) async fn mark(runtime: Arc<AppRuntime<LoggingAdapter>>) {
    if crate::now_playing::remote_or_cast_active() {
        log::info!("[qbz-qt] A-B loop: ignored while playback is remote");
        return;
    }
    let event = runtime.core().player().get_playback_event();
    if event.track_id == 0 || event.duration == 0 {
        return;
    }
    let pos_ms = runtime.core().player().state.current_position_ms();
    let next = next_after_mark(current(), event.track_id, pos_ms);
    log::info!("[qbz-qt] A-B loop: mark at {pos_ms} ms -> {next:?}");
    set(next);
    if matches!(next, Some(Loop { b_ms: Some(_), .. })) {
        spawn_watcher(runtime);
    }
}

fn spawn_watcher(runtime: Arc<AppRuntime<LoggingAdapter>>) {
    let my_gen = GENERATION.load(Ordering::SeqCst);
    crate::spawn(async move {
        let mut ticker = tokio::time::interval(WATCH_INTERVAL);
        loop {
            ticker.tick().await;
            if GENERATION.load(Ordering::SeqCst) != my_gen {
                return;
            }
            let Some(l) = current() else { return };
            if l.b_ms.is_none() {
                return;
            }
            if crate::now_playing::remote_or_cast_active() {
                clear();
                return;
            }
            let event = runtime.core().player().get_playback_event();
            if event.track_id != l.track_id {
                clear();
                return;
            }
            if !event.is_playing {
                continue;
            }
            if should_jump(&l, runtime.core().player().state.current_position_ms()) {
                crate::playback_qt::seek_local_secs(&runtime, l.a_secs).await;
            }
        }
    });
}

/// Poll-loop track edge (and stop: `track_id == 0`).
pub(crate) fn on_track_changed(track_id: u64) {
    if let Some(l) = current() {
        if l.track_id != track_id {
            clear();
        }
    }
}

/// Every user seek funnels through `playback_qt::seek_frac`; the watcher's
/// own jump uses `seek_local_secs` directly and never reaches here.
pub(crate) fn on_manual_seek(target_secs: u64) {
    if let Some(l) = current() {
        if !survives_manual_seek(&l, target_secs) {
            clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mark_arms_a_then_b_then_clears() {
        let armed = next_after_mark(None, 7, 12_400);
        assert_eq!(
            armed,
            Some(Loop {
                track_id: 7,
                a_secs: 12,
                b_ms: None
            })
        );
        // B too close to A is ignored (the loop stays armed on A).
        assert_eq!(next_after_mark(armed, 7, 13_900), armed);
        let active = next_after_mark(armed, 7, 30_250);
        assert_eq!(
            active,
            Some(Loop {
                track_id: 7,
                a_secs: 12,
                b_ms: Some(30_250)
            })
        );
        assert_eq!(next_after_mark(active, 7, 40_000), None);
        // A different track always starts over.
        assert_eq!(
            next_after_mark(active, 8, 5_000),
            Some(Loop {
                track_id: 8,
                a_secs: 5,
                b_ms: None
            })
        );
    }

    #[test]
    fn the_watcher_jumps_just_before_b_and_a_manual_seek_outside_the_section_clears() {
        let l = Loop {
            track_id: 7,
            a_secs: 12,
            b_ms: Some(30_250),
        };
        assert!(!should_jump(&l, 29_000));
        assert!(should_jump(&l, 30_200));
        assert!(should_jump(&l, 31_000));
        assert!(survives_manual_seek(&l, 12));
        assert!(survives_manual_seek(&l, 25));
        assert!(!survives_manual_seek(&l, 11));
        assert!(!survives_manual_seek(&l, 31));
        let armed = Loop {
            track_id: 7,
            a_secs: 12,
            b_ms: None,
        };
        assert!(survives_manual_seek(&armed, 20));
        assert!(!survives_manual_seek(&armed, 3));
    }

    #[test]
    fn state_codes() {
        assert_eq!(state_code(None), 0);
        assert_eq!(
            state_code(Some(Loop {
                track_id: 1,
                a_secs: 0,
                b_ms: None
            })),
            1
        );
        assert_eq!(
            state_code(Some(Loop {
                track_id: 1,
                a_secs: 0,
                b_ms: Some(5_000)
            })),
            2
        );
    }
}
