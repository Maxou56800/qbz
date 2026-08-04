//! Per-user Intelligent Search lifecycle + access wrapper (Phase 4 bridge).
//!
//! A process-global singleton over the headless
//! `qbz_app::settings::search_service::SearchService` (ADR-006: the cache
//! (Capa A) + ranking (Capa B) model logic lives in `qbz-app`; this module
//! only owns the per-user store lifecycle and the thin accessors the Slint
//! search surfaces — the cortinilla and the result-page controller — call).
//! NOTE: there is no SWR here. The result cache is written by nobody and read
//! by nobody; only the ranking half is live.
//!
//! Lifecycle mirrors `artist_blacklist` / `fav_cache` / `discover_prefs`: a
//! process-global `Mutex<Option<Service>>` bound per session via [`init`] /
//! [`teardown`], next to the other per-user stores. `SearchService` carries no
//! interior `Mutex` (the headless layer is deliberately plain); the `Mutex`
//! here is what gives the `&mut self` cache/ranking writes their exclusive
//! access. The `enabled` kill switch is an interior `AtomicBool`, so
//! [`set_enabled`] / [`is_enabled`] only need a shared `&self`.
//!
//! Fail-safe everywhere: with no session bound (`None`) every accessor behaves
//! as "disabled" — `cached`/`top_for_query` return `None`, `store`/`record`/
//! `rank_within` are no-ops, and [`is_enabled`] returns `false` so the
//! cortinilla never fires without a bound, enabled service.

use std::path::Path;
use std::sync::Mutex;

use qbz_app::settings::search_service::SearchService;

// Re-export so qbz-slint imports the action enum from ONE place.
pub use qbz_app::settings::search_service::InteractionAction;

/// Per-user search service. `None` outside an active session (online or
/// offline); every accessor reads as disabled in that window.
static SERVICE: Mutex<Option<SearchService>> = Mutex::new(None);

// ---------------------------------------------------------------------------
// Lifecycle (mirrors artist_blacklist::{init_for_user, teardown})
// ---------------------------------------------------------------------------

/// Bind the per-user search service rooted at `base_dir` (the per-user data
/// dir; each store owns its own sub-file/sub-dir underneath). Called on every
/// session activation — login, restore, AND offline entry — next to
/// `artist_blacklist::init_for_user`. `SearchService::new` never fails
/// (missing/corrupt persisted state degrades to empty), so this is
/// infallible. Idempotent: replaces any previously bound service.
///
/// `enabled` seeds the kill switch from the persisted `ui_prefs.intelligent_search`
/// preference, so the cortinilla starts in the user's last-chosen state.
pub fn init(base_dir: &Path, enabled: bool) {
    let service = SearchService::new(base_dir);
    service.set_enabled(enabled);
    if let Ok(mut guard) = SERVICE.lock() {
        *guard = Some(service);
    }
}

/// Drop the per-user search service on logout. Mirrors
/// `artist_blacklist::teardown`.
pub fn teardown() {
    if let Ok(mut guard) = SERVICE.lock() {
        *guard = None;
    }
}

// ---------------------------------------------------------------------------
// Accessors (read-as-disabled when no session is bound)
// ---------------------------------------------------------------------------

/// Run a closure against the bound service through a shared `&`, or `default`
/// when there is none / the lock is poisoned.
fn with_service<T>(default: T, f: impl FnOnce(&SearchService) -> T) -> T {
    SERVICE
        .lock()
        .ok()
        .and_then(|guard| guard.as_ref().map(f))
        .unwrap_or(default)
}

/// Run a closure against the bound service through a `&mut` (cache `put` /
/// ranking `record` need it; the `Mutex` lock provides the exclusivity). No-op
/// when there is none / the lock is poisoned.
fn with_service_mut(f: impl FnOnce(&mut SearchService)) {
    if let Ok(mut guard) = SERVICE.lock() {
        if let Some(service) = guard.as_mut() {
            f(service);
        }
    }
}

/// Flip the master kill switch on the bound service. No-op when unbound — the
/// next [`init`] re-seeds the flag from the persisted preference anyway.
/// Works through a shared `&self` (interior `AtomicBool`), so it does not need
/// the exclusive `with_service_mut` path.
pub fn set_enabled(on: bool) {
    with_service((), |s| s.set_enabled(on));
}

/// True only when a service is bound AND it is enabled. The cortinilla gates
/// on this (fail-safe `false` when no session is bound).
pub fn is_enabled() -> bool {
    with_service(false, |s| s.enabled())
}

/// Cached merged result for `query`, or `None` when unbound / disabled /
/// uncached.
pub fn cached(query: &str) -> Option<qbz_models::SearchAllResults> {
    with_service(None, |s| s.cached(query))
}

// `store` — REMOVED with its only caller (`search.rs`'s cortinilla load).
// `SearchCache` and `cached()` stay in `qbz-app` as an unused, tested library:
// the code is fine, it was simply never wired to a reader. Anything that
// revives it should read the two traps documented in the cortinilla-parity
// contract first (a partial `Some` when the volatile tier was evicted but the
// artist slice survived, and `page()` substituting `items.len()` for the
// server's total).

/// Record a user interaction with a search-surfaced entity. No-op when
/// unbound / disabled. `kind` is one of `"artist" | "album" | "track" | "playlist"`.
pub fn record(query: &str, kind: &str, id: &str, action: InteractionAction) {
    with_service_mut(|s| s.record_interaction(query, kind, id, action));
}

/// The single highest-scored `(kind, id)` learned for `query`, or `None` when
/// unbound / disabled / nothing learned.
pub fn top_for_query(query: &str) -> Option<(String, String)> {
    with_service(None, |s| s.top_for_query(query))
}

/// Stable-sort `items` in place by their learned score for `query` (the
/// cortinilla reorder). No-op when unbound / disabled.
pub fn rank_within<T>(query: &str, kind: &str, items: &mut Vec<T>, id_of: impl Fn(&T) -> String) {
    with_service((), |s| s.rank_within(query, kind, items, id_of));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Unique temp dir under the system temp root (no `tempfile` dev-dep on
    /// qbz-slint). Created here, removed at the end of the test.
    fn unique_temp_dir() -> std::path::PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("qbz-slint-search-service-test-{nanos}"));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    /// One combined test: the singleton is process-global, so splitting into
    /// parallel tests would let them clobber each other. Covers the full
    /// round-trip: fail-safe-disabled before init, enabled gate after init,
    /// store/cached round-trip, record/top, the kill switch, then teardown
    /// restoring the fail-safe state.
    #[test]
    fn lifecycle_roundtrip() {
        let dir = unique_temp_dir();

        // Fail-safe before any session is bound.
        assert!(!is_enabled(), "no session => reads as disabled");
        assert!(cached("Pink Floyd").is_none(), "no session => no cache");
        assert!(top_for_query("Pink Floyd").is_none(), "no session => no top");

        init(&dir, true);
        assert!(is_enabled(), "init(enabled=true) => enabled");

        // record -> top_for_query returns it (cache store needs a real
        // SearchAllResults; the ranking path is enough to prove wiring).
        record("Pink Floyd", "artist", "100", InteractionAction::Favorite);
        assert_eq!(
            top_for_query("Pink Floyd"),
            Some(("artist".to_string(), "100".to_string()))
        );

        // Kill switch flips the bound service.
        set_enabled(false);
        assert!(!is_enabled(), "kill switch disables");
        assert!(top_for_query("Pink Floyd").is_none(), "disabled => no top");
        set_enabled(true);
        assert!(is_enabled(), "re-enabled");

        teardown();
        assert!(!is_enabled(), "fail-safe disabled after teardown");
        assert!(top_for_query("Pink Floyd").is_none(), "no top after teardown");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
