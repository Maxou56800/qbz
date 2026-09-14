//! QBZ Cache - Audio caching system
//!
//! Provides two-level caching for audio data:
//! - **L1 (Memory)**: In-memory LRU cache (~400MB, fast access)
//! - **L2 (Disk)**: Disk-based playback cache (~800MB, persistent)
//!
//! ## Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────┐
//! │                    Audio Request                         │
//! └─────────────────────────────────────────────────────────┘
//!                           │
//!                           ▼
//! ┌─────────────────────────────────────────────────────────┐
//! │              L1 Memory Cache (AudioCache)                │
//! │  - Fast HashMap + LRU tracking                           │
//! │  - ~400MB limit (3-4 Hi-Res tracks)                      │
//! │  - Evicted tracks spill to L2                            │
//! └─────────────────────────────────────────────────────────┘
//!                           │ miss/evict
//!                           ▼
//! ┌─────────────────────────────────────────────────────────┐
//! │              L2 Disk Cache (PlaybackCache)               │
//! │  - File-based storage                                    │
//! │  - ~800MB limit                                          │
//! │  - LRU eviction by access time                           │
//! └─────────────────────────────────────────────────────────┘
//!                           │ miss
//!                           ▼
//!                      [ Network ]
//! ```
//!
//! ## Usage
//!
//! ```ignore
//! use qbz_cache::{AudioCache, PlaybackCache};
//! use std::sync::Arc;
//!
//! // Create L2 disk cache
//! let playback_cache = Arc::new(PlaybackCache::new(800 * 1024 * 1024).unwrap());
//!
//! // Create L1 memory cache with L2 spillover
//! let audio_cache = AudioCache::with_playback_cache(
//!     400 * 1024 * 1024,
//!     playback_cache.clone()
//! );
//!
//! // Insert track
//! audio_cache.insert(12345, audio_data);
//!
//! // Retrieve track (checks L1, then L2 if configured)
//! if let Some(cached) = audio_cache.get(12345) {
//!     // Use cached.data
//! }
//! ```

mod audio_cache;
mod quality;
pub use quality::CacheQuality;
mod memory;
mod image_cache;
mod playback_cache;

pub use audio_cache::{AudioCache, CacheStats, CachedTrack, CacheAdmission};
pub use image_cache::{EvictBatch, ImageCacheService, ImageCacheStats};
pub use playback_cache::{PlaybackCache, PlaybackCacheStats};
