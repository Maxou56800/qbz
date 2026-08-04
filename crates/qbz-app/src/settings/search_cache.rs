//! CAPA A — stale-while-revalidate result cache for Intelligent Search.
//!
//! Frontend-agnostic (ADR-006) cache layer for combined search results.
//! It owns ONLY the caching of `SearchAllResults` and exposes a tiny,
//! synchronous `get`/`put` surface. It does NOT hold a `QbzCore`, does NOT
//! call `core.search_all`, and knows nothing about the live network fetch.
//!
//! **STATUS: this module is an unused library.** The SWR orchestration it was
//! written for was never wired — `get` has no production caller in either
//! frontend, and as of 2026-08-03 `put` has none either. It is kept, and kept
//! tested, because the code is correct; it simply has no reader. The Qt
//! frontend caches the FINISHED cortinilla payload instead
//! (`qbz-app/src/settings/payload_cache.rs`), which also skips the mapping,
//! ranking and local-library work an API-shaped cache cannot.
//!
//! ## Two tiers, by volatility
//!
//! - **Volatile (albums / tracks / playlists):** an in-memory, insertion-order
//!   LRU bounded to [`VOLATILE_CACHE_CAPACITY`] queries. These categories are
//!   new-release-sensitive, so they are intentionally NOT persisted — a fresh
//!   app launch starts them empty and the first live fetch repopulates them.
//! - **Persisted (artists):** a small JSON store (`<base>/search_artist_cache.json`)
//!   mapping a normalized query → its cached `Vec<Artist>`. Artists change far
//!   less often than album/track/playlist catalogs, so persisting them lets a
//!   repeated query return its artist slice instantly across restarts. The store
//!   degrades gracefully: a missing or corrupt file simply starts empty and is
//!   overwritten on the next `put`, never a panic.
//!
//! ## The cache key
//!
//! [`normalize_query`] is THE single source of truth for the key: lowercased,
//! trimmed, with internal runs of whitespace collapsed to single spaces. Both
//! the volatile LRU and the persisted artist store key on the same normalized
//! string, and `search_service.rs` / `search_ranking.rs` re-use this function
//! (there is exactly ONE definition, here).

use qbz_models::{Album, Artist, Playlist, SearchAllResults, SearchResultsPage, Track};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Tunables
// ---------------------------------------------------------------------------

/// Max distinct queries held in the volatile (album/track/playlist) LRU.
/// The spec calls for "~40 queries"; the oldest is evicted past this bound.
pub const VOLATILE_CACHE_CAPACITY: usize = 40;

/// On-disk filename for the persisted artist slice (under the per-user base dir).
pub const ARTIST_CACHE_FILE: &str = "search_artist_cache.json";

// ---------------------------------------------------------------------------
// Key normalization — THE cache key
// ---------------------------------------------------------------------------

/// Normalize a raw query into the canonical cache key: lowercase, trimmed,
/// internal whitespace runs collapsed to single spaces.
///
/// This is the ONLY definition of the cache key; `search_service.rs` and
/// `search_ranking.rs` import it from here.
pub fn normalize_query(q: &str) -> String {
    q.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

// ---------------------------------------------------------------------------
// Volatile slice (albums / tracks / playlists) held per query
// ---------------------------------------------------------------------------

/// The non-persisted, new-release-sensitive portion of a cached result.
#[derive(Debug, Clone, Default)]
struct VolatileSlice {
    albums: Vec<Album>,
    tracks: Vec<Track>,
    playlists: Vec<Playlist>,
}

// ---------------------------------------------------------------------------
// Persisted artist store (JSON file: normalized_query -> Vec<Artist>)
// ---------------------------------------------------------------------------

/// JSON-backed store for the artist slice. Models the graceful-degradation
/// discipline of `discover_prefs.rs` (never panics; a missing/corrupt file
/// yields an empty map) but is a plain `serde_json` read/write of a
/// `HashMap<normalized_query, Vec<Artist>>` rather than SQLite, since the
/// payload is a single small blob with no query needs.
struct ArtistCacheStore {
    path: PathBuf,
    entries: HashMap<String, Vec<Artist>>,
}

impl ArtistCacheStore {
    /// Open the store at `<base_dir>/search_artist_cache.json`, loading any
    /// existing entries. A missing directory is created; a missing or corrupt
    /// file degrades to an empty map (never an error to the caller).
    fn open_at(base_dir: &Path) -> Self {
        // Best-effort: if the dir can't be created the first save() will retry.
        let _ = std::fs::create_dir_all(base_dir);
        let path = base_dir.join(ARTIST_CACHE_FILE);
        let entries = Self::load_from(&path);
        Self { path, entries }
    }

    fn load_from(path: &Path) -> HashMap<String, Vec<Artist>> {
        let Ok(text) = std::fs::read_to_string(path) else {
            return HashMap::new();
        };
        serde_json::from_str::<HashMap<String, Vec<Artist>>>(&text).unwrap_or_default()
    }

    fn get(&self, key: &str) -> Option<&Vec<Artist>> {
        self.entries.get(key)
    }

    /// Upsert the artist slice for `key` and persist the whole map. A write
    /// failure is logged but never propagated (the in-memory map stays correct).
    fn put(&mut self, key: String, artists: Vec<Artist>) {
        self.entries.insert(key, artists);
        if let Err(e) = self.persist() {
            log::warn!("search_cache: failed to persist artist cache: {}", e);
        }
    }

    fn persist(&self) -> Result<(), String> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| format!("Failed to create search cache directory: {}", e))?;
        }
        let text = serde_json::to_string(&self.entries)
            .map_err(|e| format!("Failed to serialize artist cache: {}", e))?;
        std::fs::write(&self.path, text)
            .map_err(|e| format!("Failed to write artist cache: {}", e))?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// SearchCache — the public CAPA A surface
// ---------------------------------------------------------------------------

/// The result cache. Combines an in-memory LRU for the volatile categories
/// (albums / tracks / playlists) with a persisted artist store. Keyed on the
/// [`normalize_query`] of the raw query string.
pub struct SearchCache {
    /// Volatile per-query slices, keyed by normalized query.
    volatile: HashMap<String, VolatileSlice>,
    /// Insertion order of `volatile` keys (front = oldest), for LRU eviction.
    order: VecDeque<String>,
    /// Persisted artist slices.
    artists: ArtistCacheStore,
}

impl SearchCache {
    /// Open the cache rooted at `base_dir` (typically the per-user data dir).
    /// The persisted artist store is loaded; the volatile maps start empty.
    /// Never fails: a missing/corrupt artist file degrades to empty.
    pub fn new(base_dir: &Path) -> Self {
        Self {
            volatile: HashMap::new(),
            order: VecDeque::new(),
            artists: ArtistCacheStore::open_at(base_dir),
        }
    }

    /// Look up the cached merged result for `query`. Returns `None` when nothing
    /// at all is cached for the normalized key (neither artists nor volatile).
    ///
    /// When only the artist slice is cached, the album/track/playlist pages come
    /// back empty (but the result is still `Some`). When only the volatile slice
    /// is cached, the artist page is empty.
    pub fn get(&self, query: &str) -> Option<SearchAllResults> {
        let key = normalize_query(query);

        let volatile = self.volatile.get(&key);
        let cached_artists = self.artists.get(&key);

        if volatile.is_none() && cached_artists.is_none() {
            return None;
        }

        let (albums, tracks, playlists) = match volatile {
            Some(v) => (v.albums.clone(), v.tracks.clone(), v.playlists.clone()),
            None => (Vec::new(), Vec::new(), Vec::new()),
        };
        let artists = cached_artists.cloned().unwrap_or_default();

        Some(SearchAllResults {
            albums: page(albums),
            tracks: page(tracks),
            artists: page(artists),
            playlists: page(playlists),
            // most_popular is a derived hero, not cached; the controller can
            // recompute it from the live result. Cached reads return None.
            most_popular: None,
        })
    }

    /// Store `results` for `query`: the album/track/playlist items go into the
    /// volatile LRU (evicting the oldest query past the bound) and the artist
    /// items are persisted to disk. A live result always wins — any existing
    /// entry for the key is overwritten.
    pub fn put(&mut self, query: &str, results: &SearchAllResults) {
        let key = normalize_query(query);

        // --- volatile slice (LRU) ---
        let slice = VolatileSlice {
            albums: results.albums.items.clone(),
            tracks: results.tracks.items.clone(),
            playlists: results.playlists.items.clone(),
        };
        let is_new_key = !self.volatile.contains_key(&key);
        self.volatile.insert(key.clone(), slice);
        if is_new_key {
            self.order.push_back(key.clone());
        } else {
            // Refresh recency: move the key to the back (most-recent) position.
            if let Some(pos) = self.order.iter().position(|k| k == &key) {
                self.order.remove(pos);
            }
            self.order.push_back(key.clone());
        }
        self.evict_to_bound();

        // --- persisted artist slice ---
        self.artists.put(key, results.artists.items.clone());
    }

    /// Evict the oldest volatile entries until within [`VOLATILE_CACHE_CAPACITY`].
    /// Only the volatile maps are bounded; the persisted artist store is not
    /// evicted here (it is small and survives restarts by design).
    fn evict_to_bound(&mut self) {
        while self.volatile.len() > VOLATILE_CACHE_CAPACITY {
            if let Some(oldest) = self.order.pop_front() {
                self.volatile.remove(&oldest);
            } else {
                break;
            }
        }
    }
}

/// Build a `SearchResultsPage<T>` from cached items: `total` = items.len(),
/// `offset` = 0, `limit` = items.len() (a full single-page reconstruction).
fn page<T>(items: Vec<T>) -> SearchResultsPage<T> {
    let n = items.len() as u32;
    SearchResultsPage {
        items,
        total: n,
        offset: 0,
        limit: n,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_test_dir(name: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("qbz-app-{name}-{}-{nonce}", std::process::id()))
    }

    // Album/Track do not derive Default; build them via serde from a minimal
    // object (all their fields are Option or #[serde(default)]).
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

    fn results(
        albums: Vec<Album>,
        tracks: Vec<Track>,
        artists: Vec<Artist>,
        playlists: Vec<Playlist>,
    ) -> SearchAllResults {
        SearchAllResults {
            albums: page(albums),
            tracks: page(tracks),
            artists: page(artists),
            playlists: page(playlists),
            most_popular: None,
        }
    }

    // (a) put-then-get round-trips albums/tracks/playlists/artists.
    #[test]
    fn put_then_get_roundtrips_all_categories() {
        let dir = unique_test_dir("search-roundtrip");
        let mut cache = SearchCache::new(&dir);

        let r = results(
            vec![album(1), album(2)],
            vec![track(10)],
            vec![artist(100), artist(200)],
            vec![playlist(7)],
        );
        cache.put("Pink Floyd", &r);

        let got = cache.get("Pink Floyd").expect("cached entry");
        assert_eq!(got.albums.items.iter().map(|a| a.id.clone()).collect::<Vec<_>>(), vec!["1", "2"]);
        assert_eq!(got.albums.total, 2);
        assert_eq!(got.albums.offset, 0);
        assert_eq!(got.albums.limit, 2);
        assert_eq!(got.tracks.items.iter().map(|t| t.id).collect::<Vec<_>>(), vec![10]);
        assert_eq!(got.artists.items.iter().map(|a| a.id).collect::<Vec<_>>(), vec![100, 200]);
        assert_eq!(got.playlists.items.iter().map(|p| p.id).collect::<Vec<_>>(), vec![7]);
        assert!(got.most_popular.is_none());

        // Unknown key -> None.
        assert!(cache.get("nothing here").is_none());

        let _ = std::fs::remove_dir_all(dir);
    }

    // (b) LRU eviction drops the oldest beyond the bound.
    #[test]
    fn lru_evicts_oldest_beyond_bound() {
        let dir = unique_test_dir("search-lru");
        let mut cache = SearchCache::new(&dir);

        // Fill exactly to capacity with distinct volatile queries.
        for i in 0..VOLATILE_CACHE_CAPACITY {
            let q = format!("query {i}");
            cache.put(&q, &results(vec![album(i as u64)], vec![], vec![], vec![]));
        }
        // The oldest ("query 0") is still present at the bound.
        assert!(cache.volatile.contains_key(&normalize_query("query 0")));
        assert_eq!(cache.volatile.len(), VOLATILE_CACHE_CAPACITY);

        // One more distinct query evicts the oldest.
        cache.put("overflow query", &results(vec![album(999)], vec![], vec![], vec![]));
        assert_eq!(cache.volatile.len(), VOLATILE_CACHE_CAPACITY);
        assert!(!cache.volatile.contains_key(&normalize_query("query 0")));
        assert!(cache.volatile.contains_key(&normalize_query("overflow query")));

        // get() on the evicted volatile key still returns Some, because the
        // ARTIST slice persists (albums/tracks/playlists come back empty).
        let evicted = cache.get("query 0").expect("artist slice persists");
        assert!(evicted.albums.items.is_empty());

        let _ = std::fs::remove_dir_all(dir);
    }

    // (c) persisted artists survive a fresh SearchCache::new at the same base_dir.
    #[test]
    fn persisted_artists_survive_reopen() {
        let dir = unique_test_dir("search-persist");
        {
            let mut cache = SearchCache::new(&dir);
            cache.put(
                "Miles Davis",
                &results(vec![album(1)], vec![track(2)], vec![artist(42), artist(43)], vec![]),
            );
        }
        // Reopen at the same base dir: volatile is gone, artists survive.
        {
            let cache = SearchCache::new(&dir);
            let got = cache.get("Miles Davis").expect("artist slice persisted");
            assert_eq!(got.artists.items.iter().map(|a| a.id).collect::<Vec<_>>(), vec![42, 43]);
            // Volatile categories did NOT persist.
            assert!(got.albums.items.is_empty());
            assert!(got.tracks.items.is_empty());
        }
        // The on-disk file lives at <base>/search_artist_cache.json.
        assert!(dir.join(ARTIST_CACHE_FILE).exists());

        // Corrupt the file -> a fresh open degrades to empty (no panic).
        std::fs::write(dir.join(ARTIST_CACHE_FILE), "{not valid json").unwrap();
        {
            let cache = SearchCache::new(&dir);
            assert!(cache.get("Miles Davis").is_none());
        }

        let _ = std::fs::remove_dir_all(dir);
    }

    // (d) normalize_query collapses whitespace/case.
    #[test]
    fn normalize_collapses_whitespace_and_case() {
        assert_eq!(normalize_query("  Pink   Floyd  "), "pink floyd");
        assert_eq!(normalize_query("MILES\tDAVIS"), "miles davis");
        assert_eq!(normalize_query("a\n\n b"), "a b");
        assert_eq!(normalize_query("Already normal"), "already normal");
        assert_eq!(normalize_query(""), "");

        // The cache keys equivalently-normalized queries together.
        let dir = unique_test_dir("search-normkey");
        let mut cache = SearchCache::new(&dir);
        cache.put("  Pink   Floyd ", &results(vec![album(1)], vec![], vec![], vec![]));
        assert!(cache.get("pink floyd").is_some());
        assert!(cache.get("PINK FLOYD").is_some());
        let _ = std::fs::remove_dir_all(dir);
    }
}
