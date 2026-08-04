//! Per-query interaction ranking for the Intelligent Search module (Capa B).
//!
//! This is the frontend-agnostic, headless ranking layer (ADR-006). It learns,
//! per *normalized query*, which entities the user actually interacts with
//! (opens, plays, favorites) and uses that signal to reorder the **cortinilla**
//! (the inline suggestion strip) — NEVER the results page itself.
//!
//! ## Privacy
//!
//! Everything here is local: a single JSON file under the per-user data dir.
//! There is zero telemetry — no network, no analytics, no remote reporting.
//!
//! ## Persistence
//!
//! State is a `HashMap<normalized_query, HashMap<(kind, id), score>>` serialized
//! to `<base_dir>/search/search_ranking.json`. A missing or corrupt file loads
//! as empty state and never panics (same graceful-degradation discipline as
//! `discover_prefs` / `reco_store`). Writes are best-effort: a failure is logged
//! via `log::warn!` and swallowed, never propagated to the caller.
//!
//! ## Bounds (so the file can't grow unbounded)
//!
//! - Each `(kind, id)` score is capped at `MAX_SCORE` (1000).
//! - The number of distinct queries is LRU-bound to `MAX_QUERIES` (200); the
//!   least-recently-touched query is evicted when the cap is exceeded.
//!
//! ## Architecture note
//!
//! This struct owns ONLY Capa A's sibling (Capa B). It does NOT hold `QbzCore`
//! and does NOT call `search_all`. The SWR orchestration (render cached -> fire
//! live -> replace) lives in the qbz-slint controller. See the module-level
//! decision in the search module.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

// Normalization is shared with Capa A (the cache). Both files compile together.
use super::search_cache::normalize_query;

// ---------------------------------------------------------------------------
// Tunables
// ---------------------------------------------------------------------------

/// Weight added for an "open" / navigate interaction.
pub const WEIGHT_OPEN: i64 = 1;
/// Weight added for a "play" interaction.
pub const WEIGHT_PLAY: i64 = 2;
/// Weight added for a "favorite" interaction.
pub const WEIGHT_FAVORITE: i64 = 3;

/// Maximum accumulated score for any single `(kind, id)` pair. Prevents a
/// single hammered entity from dominating and bounds the on-disk integer size.
pub const MAX_SCORE: i64 = 1000;

/// Maximum number of distinct normalized queries retained. LRU-evicted.
pub const MAX_QUERIES: usize = 200;

// ---------------------------------------------------------------------------
// Interaction action
// ---------------------------------------------------------------------------

/// A user interaction with a search-surfaced entity. The weight is the score
/// increment applied to that entity for the originating query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InteractionAction {
    /// Opened / navigated to the entity (album page, artist page, ...).
    Open,
    /// Started playback of the entity.
    Play,
    /// Favorited the entity.
    Favorite,
}

impl InteractionAction {
    /// The score increment for this action.
    pub fn weight(self) -> i64 {
        match self {
            InteractionAction::Open => WEIGHT_OPEN,
            InteractionAction::Play => WEIGHT_PLAY,
            InteractionAction::Favorite => WEIGHT_FAVORITE,
        }
    }
}

// ---------------------------------------------------------------------------
// Persisted shape
// ---------------------------------------------------------------------------

/// One scored entity within a query bucket. `kind` is one of
/// `"artist" | "album" | "track" | "playlist"`; `id` is the entity id as a
/// string. We persist as a flat list (instead of a map keyed by a tuple)
/// because JSON object keys must be strings — a list of records round-trips
/// cleanly and is unambiguous.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct ScoredEntity {
    kind: String,
    id: String,
    score: i64,
}

/// One query bucket: the normalized query plus its scored entities. `order` is
/// a monotonically increasing recency stamp used for LRU eviction.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct QueryBucket {
    query: String,
    #[serde(default)]
    order: u64,
    entities: Vec<ScoredEntity>,
}

/// The full persisted document.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct RankingDoc {
    #[serde(default)]
    buckets: Vec<QueryBucket>,
}

// ---------------------------------------------------------------------------
// In-memory store
// ---------------------------------------------------------------------------

/// Per-query interaction ranking. See module docs.
pub struct SearchRanking {
    /// Path to the JSON file we read/write.
    path: PathBuf,
    /// `normalized_query -> { (kind, id) : score }`.
    ranking: HashMap<String, HashMap<(String, String), i64>>,
    /// `normalized_query -> recency stamp` (higher = more recently touched).
    order: HashMap<String, u64>,
    /// Monotonic counter feeding `order`.
    tick: u64,
    /// Serialized state waiting to be written, if any. `record` fills this
    /// instead of touching the disk (see [`SearchRanking::persist`]).
    dirty: bool,
    /// Guards the file so two background writers cannot interleave, and lets
    /// a stale one notice it has been superseded.
    writer: Arc<WriterGuard>,
}

/// Serializes background writes to one file and lets a superseded write bail.
///
/// `record` is called from UI callbacks in both frontends, i.e. the GUI
/// thread, and it used to `to_string_pretty` + `fs::write` inline. The store
/// is small (tens of KB), so this was a short stall rather than a freeze — but
/// it is still blocking file IO on the thread that paints, which the Qt port's
/// own rules forbid, and it happened on every row activation.
///
/// The write moves to a detached thread. Correctness comes from two pieces:
/// the mutex means writes never interleave, and the generation counter means a
/// writer that lost the race exits instead of clobbering newer state with
/// older bytes. Losing a race is the NORMAL case when a user clicks quickly —
/// only the last write matters, and it always wins.
#[derive(Default)]
struct WriterGuard {
    lock: Mutex<()>,
    generation: AtomicU64,
}

/// Serialize + write. The ONLY place that touches the file.
///
/// Best-effort, exactly as before: every failure is logged and swallowed,
/// because the in-memory state is the source of truth and losing a write
/// costs the tail of the learned data, never correctness.
fn write_doc(path: &Path, doc: &RankingDoc) {
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            log::warn!("search_ranking: cannot create dir {parent:?} ({e}); skipping persist");
            return;
        }
    }
    let json = match serde_json::to_string_pretty(doc) {
        Ok(j) => j,
        Err(e) => {
            log::warn!("search_ranking: serialize failed ({e}); skipping persist");
            return;
        }
    };
    if let Err(e) = std::fs::write(path, json) {
        log::warn!("search_ranking: write to {path:?} failed ({e}); state kept in memory");
    }
}

impl Drop for SearchRanking {
    /// Flush on teardown, on THIS thread.
    ///
    /// This is what bounds the window the background writer opens. A detached
    /// writer spawned moments before logout might not finish; this runs when
    /// the store is dropped (logout sets its `Option` to `None`) and takes the
    /// same lock, so the last state always reaches disk in an orderly way.
    fn drop(&mut self) {
        self.persist_blocking();
    }
}

impl SearchRanking {
    /// Load the ranking from `<base_dir>/search/search_ranking.json`.
    ///
    /// A missing or corrupt file yields an empty ranking — this never panics
    /// and never returns an error. The `search/` subdir is created lazily on
    /// the first successful save, not here.
    pub fn new(base_dir: &Path) -> Self {
        let path = base_dir.join("search").join("search_ranking.json");
        let mut store = SearchRanking {
            path,
            ranking: HashMap::new(),
            order: HashMap::new(),
            tick: 0,
            dirty: false,
            writer: Arc::new(WriterGuard::default()),
        };
        store.load();
        store
    }

    /// Read + parse the JSON file into memory. Any error degrades to empty.
    fn load(&mut self) {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(_) => return, // missing file == empty ranking (normal first run)
        };
        let doc: RankingDoc = match serde_json::from_str(&text) {
            Ok(d) => d,
            Err(e) => {
                log::warn!(
                    "search_ranking: corrupt JSON at {:?} ({e}); starting empty",
                    self.path
                );
                return;
            }
        };
        let mut max_order = 0u64;
        for bucket in doc.buckets {
            let mut map: HashMap<(String, String), i64> = HashMap::new();
            for ent in bucket.entities {
                let score = ent.score.clamp(0, MAX_SCORE);
                if score <= 0 {
                    continue;
                }
                map.insert((ent.kind, ent.id), score);
            }
            if map.is_empty() {
                continue;
            }
            max_order = max_order.max(bucket.order);
            // ONE-SHOT KEY MIGRATION (2026-08-03). `normalize_query` gained
            // accent + punctuation folding, so a bucket persisted under the
            // OLD rule can carry a key the new rule would never produce —
            // `lääz rockit` where lookups now ask for `laaz rockit`. Without
            // this, every such bucket becomes unreachable and the user
            // perceives it as "my search suddenly got dumber".
            //
            // Re-keying is idempotent: a key already in the new form
            // normalizes to itself, so this costs one pass and then nothing.
            // Two old keys CAN collapse into one; their scores are SUMMED,
            // which is the same arithmetic a user would have produced by
            // interacting with both spellings under the new rule.
            //
            // Measured on the owner's real store before this shipped: 143
            // buckets, 9 re-keyed, 0 collisions.
            let key = normalize_query(&bucket.query);
            match self.ranking.get_mut(&key) {
                Some(existing) => {
                    for (k, v) in map {
                        let slot = existing.entry(k).or_insert(0);
                        *slot = (*slot + v).min(MAX_SCORE);
                    }
                    // Keep the MORE RECENT of the two recency stamps.
                    let prev = self.order.get(&key).copied().unwrap_or(0);
                    self.order.insert(key, prev.max(bucket.order));
                }
                None => {
                    self.order.insert(key.clone(), bucket.order);
                    self.ranking.insert(key, map);
                }
            }
        }
        self.tick = max_order;
        // The re-keyed state only reaches disk on the next `record`. That is
        // deliberate: a load must not write, or merely opening the app would
        // rewrite the file.
    }

    /// Serialize the current in-memory state and write it to disk. Best-effort:
    /// failures are logged and swallowed. Creates the `search/` subdir if needed.
    /// Hand the current state to a background writer.
    ///
    /// Returns immediately: the serialize and the `fs::write` happen on a
    /// detached thread. Safe to call from a UI callback, which is exactly
    /// where `record` is called from.
    fn persist(&mut self) {
        self.dirty = true;
        let doc = self.snapshot();
        let path = self.path.clone();
        let writer = Arc::clone(&self.writer);
        let generation = writer.generation.fetch_add(1, Ordering::SeqCst) + 1;
        std::thread::spawn(move || {
            let _guard = writer.lock.lock().unwrap_or_else(|e| e.into_inner());
            // Superseded while we waited for the lock: the newer writer holds
            // strictly fresher state, so writing ours would move the file
            // BACKWARDS. Bail.
            if writer.generation.load(Ordering::SeqCst) != generation {
                return;
            }
            write_doc(&path, &doc);
        });
        self.dirty = false;
    }

    /// Write the current state on THIS thread. Used by `Drop`, where spawning
    /// is pointless because nothing would join the thread.
    fn persist_blocking(&self) {
        // Take the same lock, so a flush at teardown cannot interleave with a
        // background writer that is still running.
        let _guard = self
            .writer
            .lock
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        self.writer.generation.fetch_add(1, Ordering::SeqCst);
        write_doc(&self.path, &self.snapshot());
    }

    /// The persistable projection of the in-memory state.
    fn snapshot(&self) -> RankingDoc {
        self.build_doc()
    }

    fn build_doc(&self) -> RankingDoc {
        let mut buckets: Vec<QueryBucket> = self
            .ranking
            .iter()
            .map(|(query, map)| {
                let mut entities: Vec<ScoredEntity> = map
                    .iter()
                    .map(|((kind, id), &score)| ScoredEntity {
                        kind: kind.clone(),
                        id: id.clone(),
                        score,
                    })
                    .collect();
                // Deterministic on-disk order: highest score first, then kind/id.
                entities.sort_by(|a, b| {
                    b.score
                        .cmp(&a.score)
                        .then_with(|| a.kind.cmp(&b.kind))
                        .then_with(|| a.id.cmp(&b.id))
                });
                QueryBucket {
                    query: query.clone(),
                    order: self.order.get(query).copied().unwrap_or(0),
                    entities,
                }
            })
            .collect();
        // Stable file output: sort buckets by query name.
        buckets.sort_by(|a, b| a.query.cmp(&b.query));
        RankingDoc { buckets }
    }

    /// Touch a query's recency stamp (call when a query bucket is created or
    /// mutated). Returns the new stamp.
    fn touch(&mut self, query: &str) -> u64 {
        self.tick += 1;
        self.order.insert(query.to_string(), self.tick);
        self.tick
    }

    /// Evict the least-recently-touched query when over the LRU cap.
    fn enforce_query_cap(&mut self) {
        while self.ranking.len() > MAX_QUERIES {
            // Find the query with the smallest recency stamp.
            let victim = self
                .order
                .iter()
                .filter(|(q, _)| self.ranking.contains_key(*q))
                .min_by_key(|(_, &stamp)| stamp)
                .map(|(q, _)| q.clone());
            match victim {
                Some(q) => {
                    self.ranking.remove(&q);
                    self.order.remove(&q);
                }
                None => break, // defensive: nothing to evict
            }
        }
    }

    /// Record an interaction: bump `(kind, id)`'s score for `query` by the
    /// action weight, cap it at `MAX_SCORE`, enforce the LRU query cap, then
    /// persist (best-effort). `kind` should be one of
    /// `"artist" | "album" | "track" | "playlist"`.
    pub fn record(&mut self, query: &str, kind: &str, id: &str, action: InteractionAction) {
        let key = normalize_query(query);
        if key.is_empty() {
            return;
        }
        self.touch(&key);
        let bucket = self.ranking.entry(key.clone()).or_default();
        let slot = bucket.entry((kind.to_string(), id.to_string())).or_insert(0);
        *slot = (*slot + action.weight()).min(MAX_SCORE);
        self.enforce_query_cap();
        self.persist();
    }

    /// The single highest-scored entity for `query`, if any. Ties break
    /// deterministically: higher score, then kind ascending, then id ascending.
    pub fn top_for_query(&self, query: &str) -> Option<(String, String)> {
        let key = normalize_query(query);
        let bucket = self.ranking.get(&key)?;
        bucket
            .iter()
            .max_by(|(ak, &asc), (bk, &bsc)| {
                // We want the *max* element; for ties we prefer the lexically
                // smaller (kind, id), so invert those comparisons.
                asc.cmp(&bsc)
                    .then_with(|| bk.0.cmp(&ak.0))
                    .then_with(|| bk.1.cmp(&ak.1))
            })
            .map(|((kind, id), _)| (kind.clone(), id.clone()))
    }

    /// The learned score for a specific `(kind, id)` under `query`, or 0.
    pub fn score_for(&self, query: &str, kind: &str, id: &str) -> i64 {
        let key = normalize_query(query);
        self.ranking
            .get(&key)
            .and_then(|b| b.get(&(kind.to_string(), id.to_string())))
            .copied()
            .unwrap_or(0)
    }

    /// Stable-sort `items` in place, descending by their learned score for
    /// `(kind, id_of(item))` under `query`. Items with no learned score keep
    /// their original relative order and sit behind all scored items.
    ///
    /// This is for the **cortinilla only** — never call it to reorder the
    /// results page.
    pub fn rank_within<T>(
        &self,
        query: &str,
        kind: &str,
        items: &mut Vec<T>,
        id_of: impl Fn(&T) -> String,
    ) {
        let key = normalize_query(query);
        let bucket = match self.ranking.get(&key) {
            Some(b) if !b.is_empty() => b,
            _ => return, // nothing learned for this query: leave API order intact
        };
        // `sort_by` is stable, so equal-score items (including all unscored,
        // which share score 0) retain their original relative order. Descending
        // by score puts scored items ahead of unscored ones.
        items.sort_by(|a, b| {
            let sa = bucket
                .get(&(kind.to_string(), id_of(a)))
                .copied()
                .unwrap_or(0);
            let sb = bucket
                .get(&(kind.to_string(), id_of(b)))
                .copied()
                .unwrap_or(0);
            sb.cmp(&sa)
        });
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
        std::env::temp_dir().join(format!("qbz_search_ranking_{name}_{nonce}"))
    }

    #[test]
    fn record_bumps_by_correct_weights_and_accumulates() {
        let dir = unique_test_dir("weights");
        let mut r = SearchRanking::new(&dir);
        r.record("Pink Floyd", "artist", "42", InteractionAction::Open); // +1
        assert_eq!(r.score_for("pink floyd", "artist", "42"), 1);
        r.record("Pink Floyd", "artist", "42", InteractionAction::Play); // +2
        assert_eq!(r.score_for("Pink Floyd", "artist", "42"), 3);
        r.record("Pink Floyd", "artist", "42", InteractionAction::Favorite); // +3
        assert_eq!(r.score_for("PINK FLOYD", "artist", "42"), 6);
        // Distinct entity is tracked separately.
        r.record("Pink Floyd", "album", "99", InteractionAction::Play);
        assert_eq!(r.score_for("pink floyd", "album", "99"), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn top_for_query_returns_the_max() {
        let dir = unique_test_dir("top");
        let mut r = SearchRanking::new(&dir);
        r.record("daft punk", "artist", "1", InteractionAction::Open); // 1
        r.record("daft punk", "album", "2", InteractionAction::Favorite); // 3
        r.record("daft punk", "track", "3", InteractionAction::Play); // 2
        assert_eq!(
            r.top_for_query("Daft Punk"),
            Some(("album".to_string(), "2".to_string()))
        );
        assert_eq!(r.top_for_query("never searched"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn score_cap_holds() {
        let dir = unique_test_dir("cap");
        let mut r = SearchRanking::new(&dir);
        // 500 favorites * 3 = 1500, capped at MAX_SCORE.
        for _ in 0..500 {
            r.record("x", "track", "7", InteractionAction::Favorite);
        }
        assert_eq!(r.score_for("x", "track", "7"), MAX_SCORE);
        // Further bumps don't exceed the cap.
        r.record("x", "track", "7", InteractionAction::Play);
        assert_eq!(r.score_for("x", "track", "7"), MAX_SCORE);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn query_lru_cap_evicts_oldest() {
        let dir = unique_test_dir("lru");
        let mut r = SearchRanking::new(&dir);
        // Fill exactly to the cap.
        for i in 0..MAX_QUERIES {
            r.record(&format!("q{i}"), "artist", "1", InteractionAction::Open);
        }
        assert_eq!(r.ranking.len(), MAX_QUERIES);
        // q0 is the oldest, still present.
        assert_eq!(r.score_for("q0", "artist", "1"), 1);
        // One more distinct query evicts the oldest (q0).
        r.record("overflow", "artist", "1", InteractionAction::Open);
        assert_eq!(r.ranking.len(), MAX_QUERIES);
        assert_eq!(r.score_for("q0", "artist", "1"), 0); // evicted
        assert_eq!(r.score_for("overflow", "artist", "1"), 1); // present
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn old_keys_are_migrated_and_collisions_sum() {
        // A store written under the OLD normalize rule (accents and
        // punctuation preserved) must stay reachable under the new one.
        let dir = unique_test_dir("migrate");
        std::fs::create_dir_all(dir.join("search")).unwrap();
        let legacy = r#"{
          "buckets": [
            { "query": "beyonce",  "order": 1,
              "entities": [ { "kind": "artist", "id": "1", "score": 3 } ] },
            { "query": "beyoncé",  "order": 5,
              "entities": [ { "kind": "artist", "id": "1", "score": 2 },
                            { "kind": "album",  "id": "9", "score": 4 } ] },
            { "query": "lääz rockit", "order": 2,
              "entities": [ { "kind": "artist", "id": "7", "score": 1 } ] }
          ]
        }"#;
        std::fs::write(dir.join("search").join("search_ranking.json"), legacy).unwrap();

        let r = SearchRanking::new(&dir);
        // The two spellings collapsed into ONE bucket and their scores SUMMED.
        assert_eq!(r.score_for("beyonce", "artist", "1"), 5, "3 + 2");
        assert_eq!(r.score_for("beyoncé", "artist", "1"), 5, "same bucket now");
        assert_eq!(r.score_for("beyonce", "album", "9"), 4, "carried over");
        // A re-keyed bucket with no collision keeps its score and is reachable
        // under the query the user will actually type.
        assert_eq!(r.score_for("laaz rockit", "artist", "7"), 1);
        assert_eq!(r.score_for("Lääz Rockit", "artist", "7"), 1);
        drop(r);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn background_write_lands_without_a_drop() {
        // The round-trip test above drops the store first, so it exercises the
        // Drop flush. This one proves the BACKGROUND writer actually reaches
        // disk on its own, which is the path every row click takes.
        let dir = unique_test_dir("bgwrite");
        let mut r = SearchRanking::new(&dir);
        r.record("portishead", "album", "dummy", InteractionAction::Play);
        let path = dir.join("search").join("search_ranking.json");
        // Detached thread: poll rather than sleep a fixed amount, so the test
        // is neither flaky nor slower than it has to be.
        let mut landed = false;
        for _ in 0..200 {
            if path.exists() && std::fs::read_to_string(&path).is_ok_and(|j| j.contains("dummy")) {
                landed = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(landed, "the background writer never wrote {path:?}");
        // The in-memory state is authoritative and correct IMMEDIATELY — the
        // UI never waits on the disk.
        assert_eq!(r.score_for("portishead", "album", "dummy"), 2);
        drop(r);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn persistence_round_trips() {
        let dir = unique_test_dir("persist");
        {
            let mut r = SearchRanking::new(&dir);
            r.record("radiohead", "album", "okc", InteractionAction::Favorite); // 3
            r.record("radiohead", "track", "creep", InteractionAction::Play); // 2
        }
        // Fresh instance over the SAME dir reads persisted state.
        let r2 = SearchRanking::new(&dir);
        assert_eq!(r2.score_for("radiohead", "album", "okc"), 3);
        assert_eq!(r2.score_for("radiohead", "track", "creep"), 2);
        assert_eq!(
            r2.top_for_query("Radiohead"),
            Some(("album".to_string(), "okc".to_string()))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_file_loads_empty() {
        let dir = unique_test_dir("corrupt");
        let search_dir = dir.join("search");
        std::fs::create_dir_all(&search_dir).unwrap();
        std::fs::write(search_dir.join("search_ranking.json"), b"{ not json").unwrap();
        let r = SearchRanking::new(&dir);
        assert_eq!(r.score_for("anything", "artist", "1"), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rank_within_reorders_scored_ahead_keeping_unscored_stable() {
        let dir = unique_test_dir("rank");
        let mut r = SearchRanking::new(&dir);
        // Learn scores: id "b" highest, id "d" lower, others unseen.
        r.record("metallica", "album", "b", InteractionAction::Favorite); // 3
        r.record("metallica", "album", "d", InteractionAction::Open); // 1

        // Original API order: a, b, c, d, e (a/c/e unscored).
        let mut items = vec!["a", "b", "c", "d", "e"];
        r.rank_within("metallica", "album", &mut items, |s| s.to_string());

        // Scored items first (b=3, d=1), then unscored in original order (a,c,e).
        assert_eq!(items, vec!["b", "d", "a", "c", "e"]);

        // A query with nothing learned leaves order untouched.
        let mut untouched = vec!["x", "y", "z"];
        r.rank_within("unknown", "album", &mut untouched, |s| s.to_string());
        assert_eq!(untouched, vec!["x", "y", "z"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
