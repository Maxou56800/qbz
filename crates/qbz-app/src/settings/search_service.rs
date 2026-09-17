//! SearchService — the composed, frontend-agnostic Intelligent Search facade.
//!
//! This is the single reusable entry point that owns the two headless layers of
//! Intelligent Search (ADR-006):
//!
//! - **Capa A** — [`SearchCache`]: stale-while-revalidate result cache.
//! - **Capa B** — [`SearchRanking`]: per-query interaction ranking for the
//!   cortinilla.
//!
//! ## What it deliberately does NOT own
//!
//! `SearchService` is **non-generic**. It does NOT hold a `QbzCore` and does NOT
//! call `core.search_all`. `QbzCore` is `QbzCore<A: FrontendAdapter>`; making
//! `SearchService<A>` would force that generic through every qbz-slint global
//! accessor for no benefit. NOTE: the SWR orchestration this describes was
//! never actually wired in either frontend — see `search_cache`'s header.
//! This struct is purely the
//! reusable cache + ranking layer.
//!
//! ## Interior mutability
//!
//! Cache `put` and ranking `record` need `&mut self`; the corresponding service
//! methods therefore take `&mut self`. There is intentionally NO interior
//! `Mutex` inside `SearchService` — the qbz-slint global wraps the whole service
//! in a `Mutex` (Phase 4), so the caller owns the locking. The only interior
//! mutability here is the [`AtomicBool`] enabled flag, which `set_enabled` /
//! `enabled` can flip through a shared `&self` (the kill switch must work even
//! while another thread holds nothing but `&self`).

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use super::search_cache::SearchCache;
use super::search_ranking::SearchRanking;

// Re-export so qbz-slint imports the action enum from ONE place.
pub use super::search_ranking::InteractionAction;

/// The composed Intelligent Search service: cache (Capa A) + ranking (Capa B),
/// gated by an `enabled` kill switch. Frontend-agnostic, headless, plain (no
/// interior `Mutex` around the stores — the caller locks).
pub struct SearchService {
    /// Capa A — result cache (SWR).
    cache: SearchCache,
    /// Capa B — per-query interaction ranking.
    ranking: SearchRanking,
    /// Master on/off. Default `true`. When `false`, every method is an inert
    /// no-op (`cached`/`top_for_query` return `None`; `store`/`record_interaction`/
    /// `rank_within` do nothing).
    enabled: AtomicBool,
}

impl SearchService {
    /// Construct both stores rooted at `base_dir` (typically the per-user data
    /// dir). Each store owns its own sub-file / sub-dir under that base. Never
    /// fails: missing or corrupt persisted state degrades to empty.
    pub fn new(base_dir: &Path) -> Self {
        Self {
            cache: SearchCache::new(base_dir),
            ranking: SearchRanking::new(base_dir),
            enabled: AtomicBool::new(true),
        }
    }

    /// Whether Intelligent Search is currently enabled.
    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Flip the master kill switch. Works through a shared `&self` so the toggle
    /// can be applied without taking the store lock for mutation.
    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::Relaxed);
    }

    /// Cached merged result for `query`, or `None` when disabled or uncached.
    pub fn cached(&self, query: &str) -> Option<qbz_models::SearchAllResults> {
        if !self.enabled() {
            return None;
        }
        self.cache.get(query)
    }

    /// Store a live `results` page for `query` in the cache. No-op when disabled.
    pub fn store(&mut self, query: &str, results: &qbz_models::SearchAllResults) {
        if !self.enabled() {
            return;
        }
        self.cache.put(query, results);
    }

    /// Record a user interaction with a search-surfaced entity. No-op when
    /// disabled. `kind` is one of `"artist" | "album" | "track" | "playlist"`.
    pub fn record_interaction(
        &mut self,
        query: &str,
        kind: &str,
        id: &str,
        action: InteractionAction,
    ) {
        if !self.enabled() {
            return;
        }
        self.ranking.record(query, kind, id, action);
    }

    /// The single highest-scored `(kind, id)` learned for `query`, or `None`
    /// when disabled / nothing learned.
    pub fn top_for_query(&self, query: &str) -> Option<(String, String)> {
        if !self.enabled() {
            return None;
        }
        self.ranking.top_for_query(query)
    }

    /// Stable-sort `items` in place by their learned score for `query` (the
    /// cortinilla reorder). No-op when disabled.
    pub fn rank_within<T>(
        &self,
        query: &str,
        kind: &str,
        items: &mut Vec<T>,
        id_of: impl Fn(&T) -> String,
    ) {
        if !self.enabled() {
            return;
        }
        self.ranking.rank_within(query, kind, items, id_of);
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use qbz_models::{Album, Artist, Playlist, SearchAllResults, SearchResultsPage, Track};
    use std::path::PathBuf;

    fn unique_test_dir(name: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "qbz-app-search-service-{name}-{}-{nonce}",
            std::process::id()
        ))
    }

    fn page<T>(items: Vec<T>) -> SearchResultsPage<T> {
        let n = items.len() as u32;
        SearchResultsPage {
            items,
            total: n,
            offset: 0,
            limit: n,
        }
    }

    fn album(id: u64) -> Album {
        serde_json::from_value(serde_json::json!({ "id": id.to_string() })).unwrap()
    }
    fn track(id: u64) -> Track {
        serde_json::from_value(serde_json::json!({ "id": id })).unwrap()
    }
    fn playlist(id: u64) -> Playlist {
        serde_json::from_value(serde_json::json!({ "id": id })).unwrap()
    }
    fn artist(id: u64) -> Artist {
        Artist {
            id,
            ..Default::default()
        }
    }

    fn sample_results() -> SearchAllResults {
        SearchAllResults {
            albums: page(vec![album(1)]),
            tracks: page(vec![track(10)]),
            artists: page(vec![artist(100)]),
            playlists: page(vec![playlist(7)]),
            most_popular: None,
        }
    }

    #[test]
    fn smoke_store_get_record_top_and_disable_gate() {
        let dir = unique_test_dir("smoke");
        let mut svc = SearchService::new(&dir);

        // Enabled by default.
        assert!(svc.enabled());

        // store -> cached round-trips.
        svc.store("Pink Floyd", &sample_results());
        let got = svc.cached("Pink Floyd").expect("cached entry");
        assert_eq!(
            got.albums.items.iter().map(|a| a.id.clone()).collect::<Vec<_>>(),
            vec!["1"]
        );
        assert_eq!(got.tracks.items.iter().map(|t| t.id).collect::<Vec<_>>(), vec![10]);
        assert_eq!(got.artists.items.iter().map(|a| a.id).collect::<Vec<_>>(), vec![100]);

        // record -> top_for_query returns it.
        svc.record_interaction("Pink Floyd", "artist", "100", InteractionAction::Favorite);
        assert_eq!(
            svc.top_for_query("Pink Floyd"),
            Some(("artist".to_string(), "100".to_string()))
        );

        // Disable: reads return None, writes no-op.
        svc.set_enabled(false);
        assert!(!svc.enabled());
        assert!(svc.cached("Pink Floyd").is_none());
        assert!(svc.top_for_query("Pink Floyd").is_none());

        // store/record are no-ops while disabled (no new data observed once re-enabled).
        svc.store("New Query", &sample_results());
        svc.record_interaction("Pink Floyd", "album", "1", InteractionAction::Play);

        // Re-enable: the data written while disabled was never stored.
        svc.set_enabled(true);
        assert!(svc.cached("New Query").is_none());
        // The pre-disable artist interaction survives; the disabled-period album bump did not.
        assert_eq!(
            svc.top_for_query("Pink Floyd"),
            Some(("artist".to_string(), "100".to_string()))
        );

        // rank_within is a no-op while disabled (order preserved).
        svc.set_enabled(false);
        let mut items = vec!["x", "y", "z"];
        svc.rank_within("Pink Floyd", "artist", &mut items, |s| s.to_string());
        assert_eq!(items, vec!["x", "y", "z"]);

        let _ = std::fs::remove_dir_all(dir);
    }
}
