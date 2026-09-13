//! L1 Memory Cache - In-memory LRU cache for audio data
//!
//! Fast access cache with configurable size limit and LRU eviction.
//! Evicted tracks can optionally spill to L2 disk cache.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::{memory::MemoryHeadroom, PlaybackCache};
use qbz_models::playback_cache::PlaybackCacheSettings;

/// Cached audio data for a track
#[derive(Clone)]
pub struct CachedTrack {
    pub track_id: u64,
    pub data: Vec<u8>,
    pub size_bytes: usize,
}

#[derive(Clone)]
struct SharedTrack {
    track_id: u64,
    data: Arc<Vec<u8>>,
    size_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CacheAdmission {
    Memory,
    Disk,
    Skipped,
}

/// Internal cache state - all in one struct to avoid deadlocks
struct CacheState {
    /// Cached tracks keyed by track ID
    tracks: HashMap<u64, SharedTrack>,
    policy: PlaybackCacheSettings,
    base: usize,
    ceiling: usize,
    effective: usize,
    headroom: Option<(MemoryHeadroom, Instant)>,
    growth_since_sample: usize,
    last_demand: Instant,
    /// Order of access for LRU eviction (most recent at back)
    access_order: Vec<u64>,
    /// Current cache size in bytes
    current_size: usize,
    /// Track IDs currently being fetched
    fetching: HashSet<u64>,
    /// Track IDs whose last prefetch failed, with when it failed. Lets the
    /// prefetch scheduler back off a track that is currently un-fetchable
    /// (e.g. the account is being 403'd) instead of re-hammering it every
    /// queue tick and feeding a request storm (issue #637).
    failed: HashMap<u64, Instant>,
}

/// Audio cache manager with LRU eviction and optional disk spillover
///
/// Provides fast in-memory caching with automatic eviction when the
/// size limit is reached. Evicted tracks are written to the L2 disk
/// cache (if configured) for later retrieval.
pub struct AudioCache {
    state: Mutex<CacheState>,
    maintenance_thread: OnceLock<std::thread::Thread>,
    /// Maximum cache size in bytes
    max_size_bytes: usize,
    /// Optional disk-based L2 cache for evicted tracks
    playback_cache: Option<Arc<PlaybackCache>>,
}

impl Default for AudioCache {
    fn default() -> Self {
        Self::new(400 * 1024 * 1024) // 400MB for ~4-5 Hi-Res tracks
    }
}

impl AudioCache {
    /// Create a new cache with specified max size in bytes
    pub fn new(max_size_bytes: usize) -> Self {
        Self {
            maintenance_thread: OnceLock::new(),
            state: Mutex::new(CacheState {
                tracks: HashMap::new(),
                policy: PlaybackCacheSettings::default(),
                base: max_size_bytes,
                ceiling: max_size_bytes,
                effective: max_size_bytes,
                headroom: None,
                growth_since_sample: 0,
                last_demand: Instant::now(),
                access_order: Vec::new(),
                current_size: 0,
                fetching: HashSet::new(),
                failed: HashMap::new(),
            }),
            max_size_bytes,
            playback_cache: None,
        }
    }

    /// Create cache with disk spillover enabled
    pub fn with_playback_cache(max_size_bytes: usize, playback_cache: Arc<PlaybackCache>) -> Self {
        Self {
            maintenance_thread: OnceLock::new(),
            state: Mutex::new(CacheState {
                tracks: HashMap::new(),
                policy: PlaybackCacheSettings::default(),
                base: max_size_bytes,
                ceiling: max_size_bytes,
                effective: max_size_bytes,
                headroom: None,
                growth_since_sample: 0,
                last_demand: Instant::now(),
                access_order: Vec::new(),
                current_size: 0,
                fetching: HashSet::new(),
                failed: HashMap::new(),
            }),
            max_size_bytes,
            playback_cache: Some(playback_cache),
        }
    }

    /// Set the playback cache for disk spillover
    pub fn set_playback_cache(&mut self, cache: Arc<PlaybackCache>) {
        self.playback_cache = Some(cache);
    }

    /// Get the playback cache reference
    pub fn get_playback_cache(&self) -> Option<&Arc<PlaybackCache>> {
        self.playback_cache.as_ref()
    }

    /// Get a track from cache if available
    pub fn get(&self, track_id: u64) -> Option<CachedTrack> {
        let mut state = self.state.lock().unwrap();

        let track = state.tracks.get(&track_id).cloned();

        if track.is_some() {
            // Update access order (move to back = most recently used)
            state.access_order.retain(|&id| id != track_id);
            state.access_order.push(track_id);
            log::debug!("Cache hit for track {}", track_id);
        } else {
            log::debug!("Cache miss for track {}", track_id);
        }

        drop(state);
        // Legacy Vec consumers copy after releasing the cache lock.
        track.map(|t| CachedTrack {
            track_id: t.track_id,
            data: (*t.data).clone(),
            size_bytes: t.size_bytes,
        })
    }

    /// Check if a track is in cache without updating access order
    pub fn contains(&self, track_id: u64) -> bool {
        self.state.lock().unwrap().tracks.contains_key(&track_id)
    }

    /// Check if a track is currently being fetched
    pub fn is_fetching(&self, track_id: u64) -> bool {
        self.state.lock().unwrap().fetching.contains(&track_id)
    }

    /// Mark a track as being fetched
    pub fn mark_fetching(&self, track_id: u64) {
        self.state.lock().unwrap().fetching.insert(track_id);
    }

    /// Unmark a track as being fetched
    pub fn unmark_fetching(&self, track_id: u64) {
        self.state.lock().unwrap().fetching.remove(&track_id);
    }

    /// Record that a prefetch for this track failed (starts a back-off window).
    pub fn mark_failed(&self, track_id: u64) {
        self.state
            .lock()
            .unwrap()
            .failed
            .insert(track_id, Instant::now());
    }

    /// True if the track failed to prefetch within `cooldown` — the scheduler
    /// uses this to skip re-hammering a currently un-fetchable track (issue
    /// #637). Expired entries are cleaned up on read.
    pub fn recently_failed(&self, track_id: u64, cooldown: Duration) -> bool {
        let mut state = self.state.lock().unwrap();
        match state.failed.get(&track_id) {
            Some(when) if when.elapsed() < cooldown => true,
            Some(_) => {
                state.failed.remove(&track_id);
                false
            }
            None => false,
        }
    }

    /// Clear a track's failure marker (e.g. once it is successfully cached).
    pub fn clear_failed(&self, track_id: u64) {
        self.state.lock().unwrap().failed.remove(&track_id);
    }

    /// Apply a validated persisted policy without restarting the audio device.
    pub fn configure(&self, policy: PlaybackCacheSettings) -> Result<(), String> {
        policy.validate()?;
        let retired = {
            let mut state = self.state.lock().unwrap();
            if state.policy == policy {
                return Ok(());
            }
            let (base, ceiling) = policy.budgets(self.max_size_bytes);
            state.policy = policy;
            state.base = base;
            state.ceiling = ceiling;
            state.effective = base;
            state.last_demand = Instant::now();
            Self::trim(&mut state)
        };
        if let Some(worker) = self.maintenance_thread.get() {
            worker.unpark();
        }
        drop(retired); // Destruction of large buffers never holds the cache mutex.
        Ok(())
    }

    /// Weak ownership: maintenance cannot keep a player/cache alive after shutdown.
    pub fn start_maintenance(cache: &Arc<Self>) {
        let weak = Arc::downgrade(cache);
        let worker = std::thread::spawn(move || loop {
            let Some(cache) = weak.upgrade() else { break };
            let headroom = if cache.stats().dynamic {
                crate::memory::read()
            } else {
                None
            };
            cache.maintain(headroom, Instant::now());
            drop(cache);
            std::thread::park_timeout(Duration::from_secs(15));
        });
        let _ = cache.maintenance_thread.set(worker.thread().clone());
    }

    fn reserve(memory: MemoryHeadroom) -> u64 {
        (256 * 1024 * 1024).max(memory.total / 4)
    }

    fn critical(memory: MemoryHeadroom) -> bool {
        memory.available < (128 * 1024 * 1024).max(memory.total / 100)
    }

    fn maintain(&self, headroom: Option<MemoryHeadroom>, now: Instant) {
        let retired = {
            let mut state = self.state.lock().unwrap();
            state.headroom = headroom.map(|m| (m, now));
            state.growth_since_sample = 0;
            if state.policy.dynamic {
                if headroom.is_some_and(|m| m.available < Self::reserve(m)) {
                    // Relinquish growth first; critical pressure also overrides the base.
                    state.effective = if headroom.is_some_and(Self::critical) {
                        0
                    } else {
                        state.base
                    };
                } else if now.saturating_duration_since(state.last_demand)
                    >= Duration::from_secs(120)
                {
                    state.effective = state.base;
                }
            }
            Self::trim(&mut state)
        };
        drop(retired);
    }

    fn trim(state: &mut CacheState) -> Vec<SharedTrack> {
        let mut retired = Vec::new();
        while state.current_size > state.effective && !state.access_order.is_empty() {
            let oldest = state.access_order.remove(0);
            if let Some(track) = state.tracks.remove(&oldest) {
                state.current_size = state.current_size.saturating_sub(track.size_bytes);
                retired.push(track);
            }
        }
        retired
    }

    pub fn insert(&self, track_id: u64, data: Vec<u8>) -> CacheAdmission {
        self.insert_shared(track_id, Arc::new(data))
    }

    /// Admit a sealed streaming buffer without allocating a second track-sized copy.
    /// Reservation, eviction and insertion are atomic with respect to other writers.
    pub fn insert_shared(&self, track_id: u64, data: Arc<Vec<u8>>) -> CacheAdmission {
        self.admit(track_id, data, true)
    }

    /// L2 reads may warm L1, but an oversized hit must not rewrite the same file.
    pub fn promote_from_disk(&self, track_id: u64, data: Vec<u8>) -> CacheAdmission {
        self.admit(track_id, Arc::new(data), false)
    }

    /// Decide storage before a download allocates its payload. Dynamic growth
    /// uses the same fresh-headroom allowance as L1 admission, not just max MiB.
    pub fn can_buffer_in_memory(&self, size: usize) -> bool {
        let mut state = self.state.lock().unwrap();
        Self::budget_for_incoming(&mut state, size);
        let fits = size <= state.effective;
        let retired = Self::trim(&mut state);
        drop(state);
        drop(retired);
        fits
    }

    fn budget_for_incoming(state: &mut CacheState, size: usize) {
        let now = Instant::now();
        state.last_demand = now;
        if state.policy.dynamic {
            if let Some((memory, _sampled)) = state.headroom.filter(|(_, sampled)| {
                now.saturating_duration_since(*sampled) <= Duration::from_secs(30)
            }) {
                let reserve = Self::reserve(memory);
                if memory.available >= reserve {
                    let desired = state.current_size.saturating_add(size).min(state.ceiling);
                    let spare = memory
                        .available
                        .saturating_sub(reserve)
                        .min(usize::MAX as u64) as usize;
                    // Spend at most half the current headroom; decoding/prefetch need room too.
                    let permitted = state
                        .effective
                        .saturating_add((spare / 2).saturating_sub(state.growth_since_sample));
                    let next = state.effective.max(desired.min(permitted));
                    state.growth_since_sample += next.saturating_sub(state.effective);
                    state.effective = next;
                } else {
                    state.effective = if Self::critical(memory) {
                        0
                    } else {
                        state.base
                    };
                }
            }
        }
    }

    fn admit(&self, track_id: u64, data: Arc<Vec<u8>>, spill_rejected: bool) -> CacheAdmission {
        let size = data.len();
        let mut retired = Vec::new();
        let admitted = {
            let mut state = self.state.lock().unwrap();
            Self::budget_for_incoming(&mut state, size);
            if size > state.effective {
                retired.extend(Self::trim(&mut state));
                false
            } else {
                if let Some(old) = state.tracks.remove(&track_id) {
                    state.current_size = state.current_size.saturating_sub(old.size_bytes);
                    retired.push(old);
                }
                state.access_order.retain(|&id| id != track_id);
                state.current_size = state.current_size.saturating_add(size);
                retired.extend(Self::trim(&mut state));
                state.tracks.insert(
                    track_id,
                    SharedTrack {
                        track_id,
                        data: data.clone(),
                        size_bytes: size,
                    },
                );
                state.access_order.push(track_id);
                true
            }
        };
        if let Some(disk) = &self.playback_cache {
            for track in retired {
                disk.insert(track.track_id, &track.data);
            }
        }
        if admitted {
            CacheAdmission::Memory
        } else if spill_rejected
            && self
                .playback_cache
                .as_ref()
                .is_some_and(|disk| disk.insert(track_id, &data))
        {
            CacheAdmission::Disk
        } else {
            CacheAdmission::Skipped
        }
    }

    /// Clear all cached data (both L1 memory and L2 disk caches)
    pub fn clear(&self) {
        let mut state = self.state.lock().unwrap();
        state.tracks.clear();
        state.access_order.clear();
        state.current_size = 0;
        state.fetching.clear();
        state.failed.clear();
        log::info!("L1 memory cache cleared");

        // Also clear L2 disk cache if present
        if let Some(ref playback_cache) = self.playback_cache {
            playback_cache.clear();
            log::info!("L2 playback cache cleared");
        }
    }

    /// Drop every L1 in-memory entry WITHOUT touching the L2 disk cache or
    /// the fetching/failed bookkeeping. The memory-pressure watchdog's
    /// relief valve: RAM is the scarce resource under pressure, the disk
    /// files cost nothing to keep and save a re-download on recovery.
    pub fn evict_all_memory(&self) {
        let mut state = self.state.lock().unwrap();
        let dropped = state.tracks.len();
        state.tracks.clear();
        state.access_order.clear();
        state.current_size = 0;
        log::info!("L1 memory cache evicted ({dropped} tracks dropped, L2 disk cache kept)");
    }

    /// Get cache statistics
    pub fn stats(&self) -> CacheStats {
        let state = self.state.lock().unwrap();
        CacheStats {
            profile: state.policy.selected_profile().as_str().into(),
            cached_tracks: state.tracks.len(),
            current_size_bytes: state.current_size,
            max_size_bytes: state.effective,
            base_size_bytes: state.base,
            ceiling_size_bytes: state.ceiling,
            recommended_size_bytes: self.max_size_bytes,
            dynamic: state.policy.dynamic,
            fetching_count: state.fetching.len(),
        }
    }
}

/// Cache statistics
#[derive(Debug, Clone, serde::Serialize)]
pub struct CacheStats {
    pub profile: String,
    pub cached_tracks: usize,
    pub current_size_bytes: usize,
    pub max_size_bytes: usize,
    pub fetching_count: usize,
    pub base_size_bytes: usize,
    pub ceiling_size_bytes: usize,
    pub recommended_size_bytes: usize,
    pub dynamic: bool,
}

#[cfg(test)]
mod playback_policy_tests {
    use super::*;
    const MIB: usize = 1024 * 1024;
    fn dynamic() -> PlaybackCacheSettings {
        PlaybackCacheSettings {
            dynamic: true,
            min_mib: Some(16),
            max_mib: Some(64),
            ..Default::default()
        }
    }
    fn roomy() -> MemoryHeadroom {
        MemoryHeadroom {
            total: 4 << 30,
            available: 3 << 30,
        }
    }

    #[test]
    fn defaults_and_unknown_headroom_do_not_grow() {
        let cache = AudioCache::new(16 * MIB);
        cache.configure(dynamic()).unwrap();
        assert_eq!(cache.insert(1, vec![0; 20 * MIB]), CacheAdmission::Skipped);
        assert_eq!(cache.stats().max_size_bytes, 16 * MIB);
        cache.configure(PlaybackCacheSettings::default()).unwrap();
        cache.maintain(Some(roomy()), Instant::now());
        assert_eq!(cache.insert(1, vec![0; 20 * MIB]), CacheAdmission::Skipped);
    }

    #[test]
    fn on_demand_growth_shares_bytes_obeys_ceiling_and_shrinks_with_hysteresis() {
        let cache = AudioCache::new(16 * MIB);
        cache.configure(dynamic()).unwrap();
        let now = Instant::now();
        cache.maintain(Some(roomy()), now);
        let bytes = Arc::new(vec![7; 24 * MIB]);
        assert_eq!(
            cache.insert_shared(1, bytes.clone()),
            CacheAdmission::Memory
        );
        assert!(Arc::ptr_eq(
            &bytes,
            &cache.state.lock().unwrap().tracks[&1].data
        ));
        assert_eq!(cache.stats().max_size_bytes, 24 * MIB);
        cache.maintain(Some(roomy()), now + Duration::from_secs(60));
        assert_eq!(cache.stats().cached_tracks, 1);
        cache.maintain(Some(roomy()), now + Duration::from_secs(121));
        assert_eq!(cache.stats().max_size_bytes, 16 * MIB);
        assert!(!cache.contains(1));
        assert_eq!(bytes[0], 7); // eviction cannot truncate the active source
        cache.maintain(Some(roomy()), Instant::now());
        assert_eq!(cache.insert(2, vec![0; 65 * MIB]), CacheAdmission::Skipped);
        assert!(cache.stats().max_size_bytes <= 64 * MIB);
    }

    #[test]
    fn pressure_overrides_base_and_stale_metrics_cannot_grant_growth() {
        let cache = AudioCache::new(16 * MIB);
        cache.configure(dynamic()).unwrap();
        cache.maintain(Some(roomy()), Instant::now() - Duration::from_secs(31));
        assert_eq!(cache.insert(1, vec![0; 24 * MIB]), CacheAdmission::Skipped);
        cache.maintain(Some(roomy()), Instant::now());
        cache.insert(1, vec![0; 24 * MIB]);
        cache.maintain(
            Some(MemoryHeadroom {
                total: 4 << 30,
                available: 64 * MIB as u64,
            }),
            Instant::now(),
        );
        assert_eq!(cache.stats().max_size_bytes, 0);
        assert_eq!(cache.stats().current_size_bytes, 0);
        assert_eq!(cache.insert(2, vec![0; 8]), CacheAdmission::Skipped);
    }

    #[test]
    fn limited_headroom_releases_growth_but_keeps_the_base() {
        let cache = AudioCache::new(16 * MIB);
        cache.configure(dynamic()).unwrap();
        cache.maintain(Some(roomy()), Instant::now());
        cache.insert(1, vec![0; 24 * MIB]);
        cache.maintain(
            Some(MemoryHeadroom {
                total: 4 << 30,
                available: 512 << 20,
            }),
            Instant::now(),
        );
        assert_eq!(cache.stats().max_size_bytes, 16 * MIB);
        assert_eq!(cache.insert(2, vec![1; 8 * MIB]), CacheAdmission::Memory);
    }

    #[test]
    fn disk_promotion_does_not_write_rejected_bytes_back_to_disk() {
        let path =
            std::env::temp_dir().join(format!("qbz-774-promote-disk-{}", std::process::id()));
        let disk = Arc::new(PlaybackCache::with_path(path.clone(), 1024).unwrap());
        let cache = AudioCache::with_playback_cache(16, disk.clone());
        assert_eq!(
            cache.promote_from_disk(1, vec![7; 32]),
            CacheAdmission::Skipped
        );
        assert!(!disk.contains(1));
        assert_eq!(
            cache.promote_from_disk(2, vec![7; 8]),
            CacheAdmission::Memory
        );
        drop(cache);
        drop(disk);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn concurrent_oversized_streams_respect_the_disk_budget() {
        let path =
            std::env::temp_dir().join(format!("qbz-774-parallel-disk-{}", std::process::id()));
        let disk = Arc::new(PlaybackCache::with_path(path.clone(), 1024).unwrap());
        let cache = Arc::new(AudioCache::with_playback_cache(16, disk.clone()));
        let handles: Vec<_> = (0..8)
            .map(|id| {
                let cache = cache.clone();
                std::thread::spawn(move || {
                    assert_eq!(cache.insert(id, vec![id as u8; 600]), CacheAdmission::Disk);
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        assert!(disk.stats().current_size_bytes <= 1024);
        let data = (0..8).find_map(|id| disk.get(id)).unwrap();
        assert_eq!(data.len(), 600);
        assert!(data.iter().all(|b| *b == data[0]));
        drop(cache);
        drop(disk);
        std::fs::remove_dir_all(path).unwrap();
    }

    #[test]
    fn concurrent_writers_and_replacement_never_exceed_budget() {
        let cache = Arc::new(AudioCache::new(128));
        let handles: Vec<_> = (0..8)
            .map(|id| {
                let cache = cache.clone();
                std::thread::spawn(move || {
                    for _ in 0..50 {
                        cache.insert(id, vec![id as u8; 32]);
                        let stats = cache.stats();
                        assert!(stats.current_size_bytes <= stats.max_size_bytes);
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        let state = cache.state.lock().unwrap();
        assert_eq!(
            state.current_size,
            state.tracks.values().map(|t| t.size_bytes).sum::<usize>()
        );
        assert_eq!(state.access_order.len(), state.tracks.len());
    }

    #[test]
    fn oversized_l1_falls_back_to_disk_and_reports_rejection_truthfully() {
        let path = std::env::temp_dir().join(format!("qbz-774-disk-{}", std::process::id()));
        let disk = Arc::new(PlaybackCache::with_path(path.clone(), 1024).unwrap());
        let cache = AudioCache::with_playback_cache(16, disk.clone());
        assert_eq!(cache.insert(1, vec![7; 32]), CacheAdmission::Disk);
        assert_eq!(disk.get(1).unwrap(), vec![7; 32]);
        assert!(!cache.contains(1));
        assert_eq!(cache.insert(2, vec![8; 2048]), CacheAdmission::Skipped);
        assert!(!disk.contains(2));
        drop(cache);
        drop(disk);
        std::fs::remove_dir_all(path).unwrap();
    }
}

#[cfg(test)]
mod memory_profile_tests {
    use super::*;
    use qbz_models::playback_cache::PlaybackMemoryProfile;
    #[test]
    fn memory_profile_switches_live_budget_and_high_growth_requires_headroom() {
        let cache=AudioCache::new(400<<20);
        let mut policy=PlaybackCacheSettings::default();policy.select_profile(PlaybackMemoryProfile::High);
        cache.configure(policy.clone()).unwrap();
        assert!(!cache.can_buffer_in_memory(681736251));
        cache.maintain(Some(MemoryHeadroom { total:4<<30, available:3<<30 }),Instant::now());
        assert!(cache.can_buffer_in_memory(681736251));
        assert_eq!(cache.stats().ceiling_size_bytes,1600<<20);
        assert!(cache.stats().max_size_bytes>400<<20);
        policy.select_profile(PlaybackMemoryProfile::Low);cache.configure(policy.clone()).unwrap();
        assert_eq!(cache.stats().max_size_bytes,50<<20);
        assert!(!cache.can_buffer_in_memory(681736251));
        assert_eq!(cache.stats().profile,"low");
        policy.dynamic=true;assert!(cache.configure(policy).is_err());
        assert_eq!(cache.stats().profile,"low");
    }
}
