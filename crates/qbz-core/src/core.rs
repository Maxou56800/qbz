//! QBZ Core Orchestrator
//!
//! The main orchestrator that connects all QBZ subsystems and provides
//! a unified API for frontends.

use std::sync::Arc;
use tokio::sync::RwLock;

use qbz_integrations::musicbrainz::cache::MusicBrainzCache;
use qbz_integrations::musicbrainz::identity::{
    self, ArtistCandidate, BoxFuture, FetchError, IsrcCredits, SceneIdentityRow,
    SceneQobuzCandidate, SourceIdentityRow,
};
use qbz_integrations::musicbrainz::genre::{extract_affinity_seeds, genre_summary, is_broad_genre};
use qbz_integrations::musicbrainz::location::compute_affinity_score;
use qbz_integrations::musicbrainz::{
    location, AffinitySeeds, AlbumAppearance, ArtistMetadata, ArtistRelationships,
    DiscoveryArtist, DiscoveryResponse, LocationCandidate, LocationDiscoveryResponse,
    MusicBrainzClient, MusicianAppearances, MusicianConfidence, Period, RelatedArtist,
    ResolvedArtist, ResolvedMusician, Tag,
};
use qbz_models::{
    ArtistStoryResponse,
    AssetOrigin, ExternalStreamAsset, StreamQualityInfo,
    Album, Artist, ArtistAlbums, CoreEvent, DiscoverAlbum, DiscoverData, DiscoverPlaylistsResponse,
    DiscoverResponse, FrontendAdapter, GenreInfo, LabelExploreResponse, LabelGetListResponse,
    LabelListPage, LabelPageData, LabelStoryResponse, PageArtistResponse,
    MostPopularItem, Playlist, PlaylistDuplicateResult, PlaylistTag, Quality, QueueState,
    QueueTrack, ReleasesGridResponse,
    RepeatMode, SearchAllResults, SearchResultsPage, StreamUrl, Track, TrackToAnalyse,
    TracksContainer, UserSession,
};
use qbz_player::{
    PlaybackState, Player, QueueAuthoritySnapshot as PlayerQueueAuthoritySnapshot, QueueManager,
};
use qbz_qobuz::QobuzClient;

use crate::error::CoreError;

/// Set of blacklisted artist ids. Built per call from the live blacklist store
/// (`qbz-app`); empty only under fail-open (no session bound / feature off).
pub type BlacklistFilter = std::collections::HashSet<u64>;

/// Set of blocked album ids (Qobuz album ids are alphanumeric `String`s, so a
/// separate, parallel axis from the `u64` artist filter). An album is hidden by
/// its OWN id regardless of artist — the surgical fix for Qobuz same-name
/// artist merges. Empty under fail-open.
pub type AlbumBlacklistFilter = std::collections::HashSet<String>;

fn parse_page<T: serde::de::DeserializeOwned>(
    value: &serde_json::Value,
    key: &str,
) -> SearchResultsPage<T> {
    // Scalars keep the old fallback semantics (missing/odd-shaped → 0, per
    // the serde defaults on SearchResultsPage), but the items array is parsed
    // PER ITEM: the previous whole-page `from_value(...).ok()` was
    // all-or-nothing, so one malformed entry blanked the entire search tab
    // (same class as favorites #556).
    let scalar = |name: &str| {
        value
            .get(key)
            .and_then(|p| p.get(name))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32
    };
    SearchResultsPage {
        items: qbz_models::lenient::parse_items_array(value, key, key),
        total: scalar("total"),
        offset: scalar("offset"),
        limit: scalar("limit"),
    }
}

/// Pick the first `most_popular` entry that survives the blacklist.
fn pick_most_popular(
    value: &serde_json::Value,
    blacklist: &BlacklistFilter,
    album_bl: &AlbumBlacklistFilter,
) -> Option<MostPopularItem> {
    let items = value.get("most_popular")?.get("items")?.as_array()?;
    for entry in items {
        let Some(kind) = entry.get("type").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(content) = entry.get("content") else {
            continue;
        };
        match kind {
            "artists" => {
                // Artists have no album id — artist axis only.
                if let Ok(a) = serde_json::from_value::<Artist>(content.clone()) {
                    if !blacklist.contains(&a.id) {
                        return Some(MostPopularItem::Artists(a));
                    }
                }
            }
            "albums" => {
                if let Ok(al) = serde_json::from_value::<Album>(content.clone()) {
                    if !album_bl.contains(&al.id) && !blacklist.contains(&al.artist.id) {
                        return Some(MostPopularItem::Albums(al));
                    }
                }
            }
            "tracks" => {
                if let Ok(t) = serde_json::from_value::<Track>(content.clone()) {
                    let blocked = t
                        .album
                        .as_ref()
                        .is_some_and(|a| album_bl.contains(&a.id))
                        || t.performer
                            .as_ref()
                            .map_or(false, |p| blacklist.contains(&p.id));
                    if !blocked {
                        return Some(MostPopularItem::Tracks(t));
                    }
                }
            }
            _ => {}
        }
    }
    None
}

/// D-FEAT: returns true if the album should be hidden by the blacklist.
///
/// Extends the historical Tauri rule (which blocked only the PRIMARY
/// `album.artist`) to also block when ANY contributor in `album.artists[]`
/// (featured artists included) is blacklisted. Centralizing this here keeps
/// every call site (search, discovery, queue-build) on ONE consistent rule.
///
/// Fail-open: an empty filter never blocks; an album with no matching id is
/// kept.
pub fn album_blacklisted(album: &Album, bl: &BlacklistFilter, album_bl: &AlbumBlacklistFilter) -> bool {
    if bl.is_empty() && album_bl.is_empty() {
        return false;
    }
    // Album axis (orthogonal): the album's OWN id being blocked hides it
    // regardless of artist.
    if album_bl.contains(&album.id) {
        return true;
    }
    if bl.contains(&album.artist.id) {
        return true;
    }
    album
        .artists
        .as_ref()
        .is_some_and(|v| v.iter().any(|a| bl.contains(&a.id)))
}

/// D-FEAT: returns true if the track should be hidden by the blacklist.
///
/// Blocks on the track's structured `performer` OR `composer` id. Extends the
/// historical Tauri rule (performer only) to also cover the composer.
///
/// Fail-open: an empty filter never blocks; a track with neither a performer
/// nor a composer id is kept (no id to match against).
///
/// D-FEAT limitation: the model exposes no structured per-track *featured
/// performer id* — only `performer`, `composer`, and a free-text `performers`
/// string. We deliberately do NOT name-match the free-text string; this rule
/// is strictly id-based.
pub fn track_blacklisted(track: &Track, bl: &BlacklistFilter, album_bl: &AlbumBlacklistFilter) -> bool {
    if bl.is_empty() && album_bl.is_empty() {
        return false;
    }
    // Album axis: a track of a blocked album is hidden too.
    if track
        .album
        .as_ref()
        .is_some_and(|a| album_bl.contains(&a.id))
    {
        return true;
    }
    track
        .performer
        .as_ref()
        .is_some_and(|a| bl.contains(&a.id))
        || track
            .composer
            .as_ref()
            .is_some_and(|a| bl.contains(&a.id))
}

/// D-FEAT: returns true if a discover-shaped album should be hidden.
///
/// Discover albums expose only a flat `artists[]` vec (no separate primary
/// `artist`), so any matching contributor id — primary or featured — blocks
/// the album. Fail-open: an empty filter never blocks.
pub fn discover_album_blacklisted(
    album: &DiscoverAlbum,
    bl: &BlacklistFilter,
    album_bl: &AlbumBlacklistFilter,
) -> bool {
    if bl.is_empty() && album_bl.is_empty() {
        return false;
    }
    if album_bl.contains(&album.id) {
        return true;
    }
    album.artists.iter().any(|a| bl.contains(&a.id))
}

/// Parse a `catalog_search` JSON payload into typed category pages,
/// dropping any item whose artist id is blacklisted and adjusting totals.
pub(crate) fn parse_search_all(
    value: &serde_json::Value,
    blacklist: &BlacklistFilter,
    album_bl: &AlbumBlacklistFilter,
) -> SearchAllResults {
    let mut albums = parse_page::<Album>(value, "albums");
    let mut tracks = parse_page::<Track>(value, "tracks");
    let mut artists = parse_page::<Artist>(value, "artists");
    let playlists = parse_page::<Playlist>(value, "playlists");

    // Artists have no album id — artist axis only.
    let before = artists.items.len();
    artists.items.retain(|a| !blacklist.contains(&a.id));
    artists.total = artists
        .total
        .saturating_sub((before - artists.items.len()) as u32);

    let before = albums.items.len();
    albums.items.retain(|al| !album_blacklisted(al, blacklist, album_bl));
    albums.total = albums
        .total
        .saturating_sub((before - albums.items.len()) as u32);

    let before = tracks.items.len();
    tracks
        .items
        .retain(|track| !track_blacklisted(track, blacklist, album_bl));
    tracks.total = tracks
        .total
        .saturating_sub((before - tracks.items.len()) as u32);

    SearchAllResults {
        albums,
        tracks,
        artists,
        playlists,
        most_popular: pick_most_popular(value, blacklist, album_bl),
    }
}

/// Core orchestrator for QBZ
///
/// This is the main entry point for any frontend (Tauri, Slint, Iced, CLI, etc.)
/// It provides a unified API and emits events through the FrontendAdapter.
pub struct QbzCore<A: FrontendAdapter> {
    /// Frontend adapter for event emission
    adapter: Arc<A>,
    /// Qobuz API client
    client: Arc<RwLock<Option<QobuzClient>>>,
    /// Queue manager
    queue: Arc<RwLock<QueueManager>>,
    /// Audio player
    player: Arc<Player>,
    /// MusicBrainz client (always present; enable/disable toggle lives inside)
    musicbrainz: Arc<MusicBrainzClient>,
    /// Persistent MB cache. Opened by the frontend (which owns the
    /// data-dir path) via `set_musicbrainz_cache`. Methods read the
    /// cache before hitting the network and persist on miss.
    musicbrainz_cache: Arc<std::sync::Mutex<Option<MusicBrainzCache>>>,
    /// Per-user artist-vector store for the playlist "Suggested Songs" engine.
    /// Opened by the frontend (owns the data dir) via `set_artist_vectors`.
    /// tokio Mutex because the suggestions engine holds it across `.await`s.
    artist_vectors: Arc<tokio::sync::Mutex<Option<qbz_reco::ArtistVectorStore>>>,
    /// Whether the core is initialized
    initialized: Arc<RwLock<bool>>,
    /// D8 guard: true when the current queue was built from an OFFLINE-ONLY
    /// local playlist — such a queue must never be pushed to the Qobuz
    /// Connect cloud. Ordinary queue replacement (`set_queue` /
    /// `set_queue_with_order` / `clear_queue`) clears it; append-style ops
    /// preserve it. An offline local-playlist replacement sets it atomically
    /// through `set_queue_with_offline_only` before publishing QueueUpdated.
    queue_offline_only: Arc<std::sync::atomic::AtomicBool>,
}

/// Exact in-process playback authority owned by [`QbzCore`].
///
/// The player snapshot preserves the queue mechanics while `offline_only`
/// preserves the cloud-publication policy coupled to that queue. Keeping both
/// opaque prevents a delegation handoff from restoring an offline playlist as
/// if it were eligible for QConnect synchronization.
#[must_use = "an authority snapshot must be retained until it is restored or deliberately discarded"]
#[derive(Clone)]
pub struct QueueAuthoritySnapshot {
    queue: PlayerQueueAuthoritySnapshot,
    offline_only: bool,
}

impl<A: FrontendAdapter + Send + Sync + 'static> QbzCore<A> {
    /// Create a new QbzCore instance with the given frontend adapter and player
    ///
    /// The Player must be created by the frontend with appropriate audio settings.
    /// QbzCore orchestrates playback through this player.
    pub fn new(adapter: A, player: Player) -> Self {
        Self {
            adapter: Arc::new(adapter),
            client: Arc::new(RwLock::new(None)),
            queue: Arc::new(RwLock::new(QueueManager::new())),
            player: Arc::new(player),
            musicbrainz: Arc::new(MusicBrainzClient::new()),
            musicbrainz_cache: Arc::new(std::sync::Mutex::new(None)),
            artist_vectors: Arc::new(tokio::sync::Mutex::new(None)),
            initialized: Arc::new(RwLock::new(false)),
            queue_offline_only: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Mark (or unmark) the current queue as built from an OFFLINE-ONLY local
    /// playlist (D8). Queue replacement code should prefer
    /// [`Self::set_queue_with_offline_only`] so the flag is visible before the
    /// `QueueUpdated` event can trigger cloud synchronization.
    fn set_queue_offline_only(&self, on: bool) {
        self.queue_offline_only
            .store(on, std::sync::atomic::Ordering::Release);
    }

    /// True when the current queue originates from an offline-only local
    /// playlist — QConnect must skip its cloud queue push.
    pub fn queue_is_offline_only(&self) -> bool {
        self.queue_offline_only
            .load(std::sync::atomic::Ordering::Acquire)
    }

    /// Install a MusicBrainz cache. The frontend owns the data path
    /// and opens the cache; QbzCore just stores the handle and uses
    /// it transparently in `musicbrainz_get_artist_metadata` and
    /// `musicbrainz_get_artist_relationships`.
    pub fn set_musicbrainz_cache(&self, cache: MusicBrainzCache) {
        if let Ok(mut guard) = self.musicbrainz_cache.lock() {
            *guard = Some(cache);
        }
    }

    /// Install the per-user artist-vector store (playlist Suggested Songs). The
    /// frontend owns the data path and opens it via
    /// `qbz_reco::ArtistVectorStore::open_at`.
    pub async fn set_artist_vectors(&self, store: qbz_reco::ArtistVectorStore) {
        *self.artist_vectors.lock().await = Some(store);
    }

    /// Drop the per-user artist-vector store on logout.
    pub async fn clear_artist_vectors(&self) {
        *self.artist_vectors.lock().await = None;
    }

    /// Initialize the core
    ///
    /// This should be called once at startup to set up all subsystems.
    /// Best-effort: if bundle token extraction fails (e.g. no network), the
    /// core still finishes initialization so queue manager and player remain
    /// usable for offline/local playback. API calls will then return
    /// `CoreError::NotInitialized` until the client is rebuilt with tokens.
    pub async fn init(&self) -> Result<(), CoreError> {
        let mut initialized = self.initialized.write().await;
        if *initialized {
            return Ok(());
        }

        let client = QobuzClient::new().map_err(|e| CoreError::Internal(e.to_string()))?;

        match client.init().await {
            Ok(_) => {
                *self.client.write().await = Some(client);
                log::info!("QbzCore initialized with bundle tokens");
            }
            Err(e) => {
                log::warn!(
                    "QbzCore: bundle token extraction failed ({}). Starting in offline-tolerant mode; API calls will be unavailable until next online start.",
                    e
                );
            }
        }

        *initialized = true;
        Ok(())
    }

    /// Whether the Qobuz API client is initialized (bundle tokens extracted).
    /// Returns false when the core is running in offline-tolerant mode after
    /// a failed bundle extraction at startup.
    pub async fn is_api_initialized(&self) -> bool {
        self.client.read().await.is_some()
    }

    /// Best-effort attempt to rebuild the Qobuz API client. Useful when the
    /// initial `init()` ran offline and the host has since regained network.
    /// No-op when the client is already initialized.
    pub async fn try_init_api(&self) -> Result<(), CoreError> {
        {
            let guard = self.client.read().await;
            if guard.is_some() {
                return Ok(());
            }
        }

        let client = QobuzClient::new().map_err(|e| CoreError::Internal(e.to_string()))?;
        client
            .init()
            .await
            .map_err(|e| CoreError::Internal(format!("Failed to extract bundle tokens: {}", e)))?;
        *self.client.write().await = Some(client);
        log::info!("QbzCore: API client initialized lazily");
        Ok(())
    }

    /// Check if a user session exists
    pub async fn has_session(&self) -> bool {
        let client = self.client.read().await;
        if let Some(c) = client.as_ref() {
            c.is_logged_in().await
        } else {
            false
        }
    }

    /// Login with email and password
    pub async fn login(&self, email: &str, password: &str) -> Result<UserSession, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;

        match client.login(email, password).await {
            Ok(session) => {
                self.emit(CoreEvent::LoggedIn {
                    session: session.clone(),
                })
                .await;
                Ok(session)
            }
            Err(e) => {
                self.emit(CoreEvent::Error {
                    code: "AUTH_FAILED".to_string(),
                    message: e.to_string(),
                    recoverable: true,
                })
                .await;
                Err(CoreError::AuthFailed(e.to_string()))
            }
        }
    }

    /// Restore a session from a saved OAuth user_auth_token.
    pub async fn login_with_token(&self, token: &str) -> Result<UserSession, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;

        match client.login_with_token(token).await {
            Ok(session) => {
                self.emit(CoreEvent::LoggedIn {
                    session: session.clone(),
                })
                .await;
                Ok(session)
            }
            Err(e) => {
                self.emit(CoreEvent::Error {
                    code: "OAUTH_TOKEN_FAILED".to_string(),
                    message: e.to_string(),
                    recoverable: true,
                })
                .await;
                // Preserve the typed ApiError: callers must distinguish an
                // explicit auth rejection (clear the saved token) from a
                // network-class failure (keep it) — stringifying here made
                // that impossible and caused the token-clearing-on-boot bug.
                Err(CoreError::Api(e))
            }
        }
    }

    /// Inject an already-authenticated session (e.g. from OAuth flow).
    /// Emits a LoggedIn event so the rest of the system knows auth state changed.
    pub async fn set_session(&self, session: UserSession) -> Result<(), CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;
        client.set_session(session.clone()).await;
        self.emit(CoreEvent::LoggedIn { session }).await;
        Ok(())
    }

    /// Logout the current user
    pub async fn logout(&self) -> Result<(), CoreError> {
        let client = self.client.read().await;
        if let Some(c) = client.as_ref() {
            c.logout().await;
            self.emit(CoreEvent::LoggedOut).await;
        }
        Ok(())
    }

    // ==================== Queue Operations ====================

    /// Get current queue state
    pub async fn get_queue_state(&self) -> QueueState {
        let queue = self.queue.read().await;
        queue.get_state()
    }

    /// Get all queue tracks and current index (for session persistence)
    pub async fn get_all_queue_tracks(&self) -> (Vec<QueueTrack>, Option<usize>) {
        let queue = self.queue.read().await;
        queue.get_all_tracks()
    }

    /// Capture the complete queue authority state atomically for a temporary
    /// renderer handoff. The returned payload is opaque and in-process only.
    pub async fn capture_authority_snapshot(&self) -> QueueAuthoritySnapshot {
        let queue = self.queue.read().await;
        QueueAuthoritySnapshot {
            queue: queue.capture_authority_snapshot(),
            offline_only: self.queue_is_offline_only(),
        }
    }

    /// Restore a previously captured queue authority state in one atomic,
    /// side-effect-free replacement. The authority coordinator publishes the
    /// restored projection only after sealing its origin and generation.
    pub async fn restore_authority_snapshot(&self, snapshot: QueueAuthoritySnapshot) {
        let queue = self.queue.write().await;
        self.set_queue_offline_only(snapshot.offline_only);
        queue.restore_authority_snapshot(snapshot.queue);
    }

    /// Commit an authority stamp and restore its exact queue as one synchronous
    /// visibility boundary. The outer Tokio lock is acquired with `try_write`
    /// because authority coordinators call this from their non-async commit
    /// gate. If either the queue is busy or `commit` rejects, ownership of the
    /// opaque snapshot is returned unchanged and no queue state is mutated.
    pub fn try_commit_authority_snapshot<F>(
        &self,
        snapshot: QueueAuthoritySnapshot,
        commit: F,
    ) -> Result<QueueState, QueueAuthoritySnapshot>
    where
        F: FnOnce() -> bool,
    {
        let QueueAuthoritySnapshot {
            queue: queue_snapshot,
            offline_only,
        } = snapshot;
        let Ok(queue) = self.queue.try_write() else {
            return Err(QueueAuthoritySnapshot {
                queue: queue_snapshot,
                offline_only,
            });
        };
        if !commit() {
            return Err(QueueAuthoritySnapshot {
                queue: queue_snapshot,
                offline_only,
            });
        }
        self.set_queue_offline_only(offline_only);
        queue.restore_authority_snapshot(queue_snapshot);
        Ok(queue.get_state())
    }

    /// Capture queue rows, cursor, and oldest-first playback history under one
    /// queue lock for session persistence.
    pub async fn get_persistable_queue_state(
        &self,
    ) -> (Vec<QueueTrack>, Option<usize>, Vec<usize>) {
        let queue = self.queue.read().await;
        queue.get_persistable_state()
    }

    /// Restore the oldest-first playback history after restoring its queue.
    pub async fn restore_queue_history(&self, history: Vec<usize>) {
        let queue = self.queue.write().await;
        queue.restore_history_indices(history);
        self.emit(CoreEvent::QueueUpdated {
            state: queue.get_state(),
        })
        .await;
    }

    /// Get the full queue state without the upcoming/history caps that
    /// `get_queue_state` applies. Used by clients that paginate the
    /// upcoming list and need the complete play history (Queue sidebar).
    pub async fn get_queue_state_full(&self) -> QueueState {
        let queue = self.queue.read().await;
        queue.get_state_full()
    }

    /// Set repeat mode
    pub async fn set_repeat_mode(&self, mode: RepeatMode) {
        let queue = self.queue.write().await;
        queue.set_repeat(mode.clone());
        self.emit(CoreEvent::RepeatModeChanged { mode }).await;
    }

    /// Set shuffle
    pub async fn set_shuffle(&self, enabled: bool) {
        let queue = self.queue.write().await;
        queue.set_shuffle(enabled);
        self.emit(CoreEvent::ShuffleChanged { enabled }).await;
        self.emit(CoreEvent::QueueUpdated {
            state: queue.get_state(),
        })
        .await;
    }

    /// Set shuffle mode using an authoritative order.
    pub async fn set_shuffle_with_order(&self, enabled: bool, shuffle_order: Option<Vec<usize>>) {
        let queue = self.queue.write().await;
        queue.set_shuffle_with_order(enabled, shuffle_order);
        self.emit(CoreEvent::ShuffleChanged { enabled }).await;
        self.emit(CoreEvent::QueueUpdated {
            state: queue.get_state(),
        })
        .await;
    }

    /// Toggle shuffle and return new state
    pub async fn toggle_shuffle(&self) -> bool {
        let queue = self.queue.write().await;
        let was_enabled = queue.is_shuffle();
        let new_enabled = !was_enabled;
        queue.set_shuffle(new_enabled);
        self.emit(CoreEvent::ShuffleChanged {
            enabled: new_enabled,
        })
        .await;
        self.emit(CoreEvent::QueueUpdated {
            state: queue.get_state(),
        })
        .await;
        new_enabled
    }

    /// Clear the queue. `keep_current=true` preserves the now-playing track
    /// (historical behavior); `false` wipes everything including the current
    /// slot — use when nothing is actively playing and the user wants a full
    /// reset.
    pub async fn clear_queue(&self, keep_current: bool) {
        let queue = self.queue.write().await;
        self.set_queue_offline_only(false);
        queue.clear(keep_current);
        self.emit(CoreEvent::QueueUpdated {
            state: queue.get_state(),
        })
        .await;
    }

    /// Add a track to the end of the queue
    pub async fn add_track(&self, track: QueueTrack) {
        let queue = self.queue.write().await;
        queue.add_track(track);
        self.emit(CoreEvent::QueueUpdated {
            state: queue.get_state(),
        })
        .await;
    }

    /// Add multiple tracks to the queue
    pub async fn add_tracks(&self, tracks: Vec<QueueTrack>) {
        let queue = self.queue.write().await;
        queue.add_tracks(tracks);
        self.emit(CoreEvent::QueueUpdated {
            state: queue.get_state(),
        })
        .await;
    }

    /// Add a track to play next (after current)
    pub async fn add_track_next(&self, track: QueueTrack) {
        let queue = self.queue.write().await;
        queue.add_track_next(track);
        self.emit(CoreEvent::QueueUpdated {
            state: queue.get_state(),
        })
        .await;
    }

    /// Add a track at the END of the manual block ("Play later", #442):
    /// after every play-next / play-later already queued, before the source
    /// resumes. "Add to queue" stays untouched at the absolute end.
    pub async fn add_track_later(&self, track: QueueTrack) {
        let queue = self.queue.write().await;
        queue.add_track_later(track);
        self.emit(CoreEvent::QueueUpdated {
            state: queue.get_state(),
        })
        .await;
    }

    /// Set the entire queue (replaces existing)
    pub async fn set_queue(&self, tracks: Vec<QueueTrack>, start_index: Option<usize>) {
        self.set_queue_with_offline_only(tracks, start_index, false)
            .await;
    }

    /// Replace the queue and publish its offline-only origin as one ordered
    /// operation. The flag is set before `QueueUpdated`, closing the window in
    /// which a QConnect subscriber could upload an offline-only playlist.
    pub async fn set_queue_with_offline_only(
        &self,
        tracks: Vec<QueueTrack>,
        start_index: Option<usize>,
        offline_only: bool,
    ) {
        let queue = self.queue.write().await;
        self.set_queue_offline_only(offline_only);
        queue.set_queue(tracks, start_index);
        self.emit(CoreEvent::QueueUpdated {
            state: queue.get_state(),
        })
        .await;
    }

    /// Replace queue contents and playback order atomically.
    pub async fn set_queue_with_order(
        &self,
        tracks: Vec<QueueTrack>,
        start_index: Option<usize>,
        shuffle_enabled: bool,
        shuffle_order: Option<Vec<usize>>,
    ) {
        let queue = self.queue.write().await;
        self.set_queue_offline_only(false);
        queue.set_queue_with_order(tracks, start_index, shuffle_enabled, shuffle_order);
        self.emit(CoreEvent::QueueUpdated {
            state: queue.get_state(),
        })
        .await;
    }

    /// Remove a track by index
    pub async fn remove_track(&self, index: usize) -> Option<QueueTrack> {
        let queue = self.queue.write().await;
        let removed = queue.remove_track(index);
        self.emit(CoreEvent::QueueUpdated {
            state: queue.get_state(),
        })
        .await;
        removed
    }

    /// Remove a track from the upcoming list by position
    pub async fn remove_upcoming_track(&self, upcoming_index: usize) -> Option<QueueTrack> {
        let queue = self.queue.write().await;
        let removed = queue.remove_upcoming_track(upcoming_index);
        self.emit(CoreEvent::QueueUpdated {
            state: queue.get_state(),
        })
        .await;
        removed
    }

    /// Remove every upcoming track after `upcoming_index` in the current play
    /// order; the track at `upcoming_index` is kept. Shuffle-aware. Emits a
    /// single `QueueUpdated` when anything was removed. Returns the count.
    pub async fn remove_upcoming_after(&self, upcoming_index: usize) -> usize {
        let queue = self.queue.write().await;
        let removed = queue.remove_upcoming_after(upcoming_index);
        if removed > 0 {
            self.emit(CoreEvent::QueueUpdated {
                state: queue.get_state(),
            })
            .await;
        }
        removed
    }

    /// Move a track from one position to another
    pub async fn move_track(&self, from_index: usize, to_index: usize) -> bool {
        let queue = self.queue.write().await;
        let success = queue.move_track(from_index, to_index);
        if success {
            self.emit(CoreEvent::QueueUpdated {
                state: queue.get_state(),
            })
            .await;
        }
        success
    }

    /// Reorder one playback-history occurrence inside Queue View. Source
    /// coordinates are most-recent-first; the destination is an oldest-first
    /// insertion slot in the chronological projection.
    pub async fn move_history_entry(
        &self,
        history_index: usize,
        expected_id: u64,
        to_slot: usize,
    ) -> bool {
        let queue = self.queue.write().await;
        let success = queue.move_history_entry(history_index, expected_id, to_slot);
        if success {
            self.emit(CoreEvent::QueueUpdated {
                state: queue.get_state(),
            })
            .await;
        }
        success
    }

    /// Move one played queue occurrence back into Upcoming without cloning
    /// it. This is the local Queue View path; remote renderers use
    /// `remove_history_entry` after their authoritative insert succeeds.
    pub async fn requeue_history_entry(
        &self,
        history_index: usize,
        expected_id: u64,
        to_slot: usize,
    ) -> bool {
        let queue = self.queue.write().await;
        let success = queue.requeue_history_entry(history_index, expected_id, to_slot);
        if success {
            self.emit(CoreEvent::QueueUpdated {
                state: queue.get_state(),
            })
            .await;
        }
        success
    }

    /// Forget the local history occurrence that an authoritative remote
    /// renderer just accepted back into its Upcoming sequence.
    pub async fn remove_history_entry(&self, history_index: usize, expected_id: u64) -> bool {
        let queue = self.queue.write().await;
        let success = queue.remove_history_entry(history_index, expected_id);
        if success {
            self.emit(CoreEvent::QueueUpdated {
                state: queue.get_state(),
            })
            .await;
        }
        success
    }

    /// Jump to a specific track by index
    pub async fn play_index(&self, index: usize) -> Option<QueueTrack> {
        let queue = self.queue.write().await;
        let track = queue.play_index(index);
        self.emit(CoreEvent::QueueUpdated {
            state: queue.get_state(),
        })
        .await;
        track
    }

    /// Jump to a track by its position in the upcoming list (as shown in the
    /// Queue sidebar). Shuffle-aware: resolves through `shuffle_order` when
    /// shuffle is active.
    pub async fn play_upcoming_at(&self, upcoming_index: usize) -> Option<QueueTrack> {
        let queue = self.queue.write().await;
        let track = queue.play_upcoming_at(upcoming_index);
        self.emit(CoreEvent::QueueUpdated {
            state: queue.get_state(),
        })
        .await;
        track
    }

    /// Move the cursor to an upcoming Listen List row while retaining every
    /// crossed row in chronological history. `expected_id` makes the snapshot
    /// index safe when queue mutations race the frontend activation.
    pub async fn play_upcoming_at_preserving_timeline(
        &self,
        upcoming_index: usize,
        expected_id: u64,
    ) -> Option<QueueTrack> {
        let queue = self.queue.write().await;
        let track = queue.play_upcoming_at_preserving_timeline(upcoming_index, expected_id);
        self.emit(CoreEvent::QueueUpdated {
            state: queue.get_state(),
        })
        .await;
        track
    }

    /// Move the cursor back to a Listen List history row without cloning it
    /// into the queue. History coordinates are most-recent-first, matching the
    /// public queue snapshot.
    pub async fn play_history_at_preserving_timeline(
        &self,
        history_index: usize,
        expected_id: u64,
    ) -> Option<QueueTrack> {
        let queue = self.queue.write().await;
        let track = queue.play_history_at_preserving_timeline(history_index, expected_id);
        self.emit(CoreEvent::QueueUpdated {
            state: queue.get_state(),
        })
        .await;
        track
    }

    /// Play `track_id` preferring an offline-cached copy (the offline tier of
    /// the shared playback tier-walk) before the player's own L1/L2 → network
    /// path. `offline` is the frontend's open `OfflineCacheState` (None = no
    /// offline tier); `sink` optionally drives the unlock animation.
    ///
    /// The player is untouched: an offline hit is handed to `play_data` (which
    /// warms L1), a miss falls through to `Player::play_track`.
    pub async fn play_track_resolved(
        &self,
        track_id: u64,
        quality: Quality,
        offline: Option<&qbz_offline_cache::OfflineCacheState>,
        sink: Option<&qbz_offline_cache::CacheEventSink>,
        start_position_secs: u64,
    ) -> Result<(), String> {
        if let Some(off) = offline {
            if let Some(bytes) =
                crate::offline_resolve::resolve_offline_bytes(track_id, off, sink).await
            {
                log::info!("[Core] track {} served from OFFLINE cache", track_id);
                let r = self.player.play_data(bytes, track_id);
                // Offline-cached bytes play from memory; honor a session-resume
                // position with a best-effort seek (0 = from the start).
                if r.is_ok() && start_position_secs > 0 {
                    let _ = self.player.seek(start_position_secs);
                }
                return r;
            }
        }
        let guard = self.client.read().await;
        let client = guard
            .as_ref()
            .ok_or_else(|| "No Qobuz client available".to_string())?;
        self.player
            .play_track(client, track_id, quality, start_position_secs)
            .await
    }

    /// Resolve the bytes for a GAPLESS successor. Tier order L1/L2 → offline →
    /// network: the offline tier is checked only when the track is NOT already
    /// in the player's cache (the CMAF decrypt is slow, ~5-7 s, so a cached
    /// copy wins). Returns bytes to hand to `Player::play_next`, or None.
    pub async fn fetch_for_gapless_resolved(
        &self,
        track_id: u64,
        quality: Quality,
        offline: Option<&qbz_offline_cache::OfflineCacheState>,
        sink: Option<&qbz_offline_cache::CacheEventSink>,
    ) -> Option<Vec<u8>> {
        if !self.player.is_track_cached(track_id) {
            if let Some(off) = offline {
                if let Some(bytes) =
                    crate::offline_resolve::resolve_offline_bytes(track_id, off, sink).await
                {
                    return Some(bytes);
                }
            }
        }
        let guard = self.client.read().await;
        let client = guard.as_ref()?;
        self.player.fetch_for_gapless(client, track_id, quality).await
    }

    /// Progressive counterpart of `fetch_for_external_stream_resolved` for a
    /// track that is NOT already cached: the bytes are served to the renderer
    /// while they download (see `Player::open_external_stream`).
    pub async fn open_external_stream_resolved(
        &self,
        track_id: u64,
        quality: Quality,
    ) -> Result<qbz_player::ExternalStreamHandle, String> {
        let guard = self.client.read().await;
        let client = guard
            .as_ref()
            .ok_or_else(|| "No Qobuz client available".to_string())?;
        self.player
            .open_external_stream(client, track_id, quality)
            .await
    }

    /// Queue a cold Qobuz successor from its initial CMAF buffer instead of
    /// materializing the whole file. Used only by the Streaming-only gapless
    /// handoff; cached and offline successors keep the byte path above.
    pub async fn queue_gapless_streaming(
        &self,
        track_id: u64,
        quality: Quality,
    ) -> Result<(), String> {
        let guard = self.client.read().await;
        let client = guard
            .as_ref()
            .ok_or_else(|| "No Qobuz client available".to_string())?;
        self.player
            .queue_next_streaming(client, track_id, quality)
            .await
    }

    /// Resolve a fully-materialized audio asset (bytes + MIME + quality) for an
    /// EXTERNAL renderer (Chromecast / DLNA). Tier order mirrors
    /// `fetch_for_gapless_resolved`: L1/L2 player cache -> OFFLINE (local CMAF
    /// decrypt, no network) -> network. The offline tier is what makes a
    /// downloaded track cast fast with no connection (the same "local segments,
    /// decrypt on demand" path the offline cache uses for playback). On a cache
    /// or offline hit the precise delivered quality isn't known here — the Cast
    /// service derives the quality label from the track's catalog metadata.
    pub async fn fetch_for_external_stream_resolved(
        &self,
        track_id: u64,
        quality: Quality,
        offline: Option<&qbz_offline_cache::OfflineCacheState>,
        sink: Option<&qbz_offline_cache::CacheEventSink>,
    ) -> Option<ExternalStreamAsset> {
        // L1/L2 player cache (decrypted FLAC) is handled inside
        // fetch_for_external_stream; only reach for the offline tier when the
        // track is not already cached (the CMAF decrypt is slow).
        if !self.player.is_track_cached(track_id) {
            if let Some(off) = offline {
                if let Some(bytes) =
                    crate::offline_resolve::resolve_offline_bytes(track_id, off, sink).await
                {
                    log::info!("[CAST-FETCH] track {track_id} served from OFFLINE cache");
                    return Some(ExternalStreamAsset {
                        bytes,
                        content_type: "audio/flac".to_string(),
                        quality: StreamQualityInfo::from_raw(0, None, None),
                        duration_secs: None,
                        origin: AssetOrigin::Offline,
                    });
                }
            }
        }
        let guard = self.client.read().await;
        let client = guard.as_ref()?;
        self.player
            .fetch_for_external_stream(client, track_id, quality)
            .await
    }

    /// Advance to next track in queue
    pub async fn next_track(&self) -> Option<QueueTrack> {
        let queue = self.queue.write().await;
        let track = queue.next();
        self.emit(CoreEvent::QueueUpdated {
            state: queue.get_state(),
        })
        .await;
        track
    }

    /// Go to previous track in queue
    pub async fn previous_track(&self) -> Option<QueueTrack> {
        let queue = self.queue.write().await;
        let track = queue.previous();
        self.emit(CoreEvent::QueueUpdated {
            state: queue.get_state(),
        })
        .await;
        track
    }

    /// Get multiple upcoming tracks without advancing (for prefetching)
    pub async fn peek_upcoming(&self, count: usize) -> Vec<QueueTrack> {
        let queue = self.queue.read().await;
        queue.peek_upcoming(count)
    }

    /// Inspect the previous queue row without changing the cursor/history.
    pub async fn peek_previous_track(&self) -> Option<QueueTrack> {
        let queue = self.queue.read().await;
        queue.peek_previous()
    }

    /// The current queue track, if any (source-aware playback routing).
    pub async fn current_track(&self) -> Option<QueueTrack> {
        let queue = self.queue.read().await;
        queue.current()
    }

    /// Set the "stop after this song" marker on a queue track id. Replaces any
    /// previous marker (single marker). Silent no-op if the id is not in the queue.
    /// Intentionally does NOT emit `CoreEvent::QueueUpdated` — the marker is a UI
    /// intent the frontend reflects via its own queue snapshot, and emitting here
    /// risks QConnect echo loops.
    pub async fn set_stop_after(&self, track_id: u64) {
        self.queue.write().await.set_stop_after(track_id);
    }

    /// Clear the "stop after" marker (user cancellation).
    pub async fn clear_stop_after(&self) {
        self.queue.write().await.clear_stop_after();
    }

    /// Read the current "stop after" marker, if any.
    pub async fn get_stop_after(&self) -> Option<u64> {
        self.queue.read().await.get_stop_after()
    }

    /// One-shot consume: if `finished_track_id` matches the marker, clear it and
    /// return true (the auto-advance driver then halts instead of advancing).
    /// Only the natural end-of-track path may call this — never a manual skip.
    pub async fn consume_stop_after_if(&self, finished_track_id: u64) -> bool {
        self.queue.write().await.consume_stop_after_if(finished_track_id)
    }

    /// Reconcile the queue pointer to the track the audio engine is actually
    /// playing. A gapless hand-off advances inside the player without going
    /// through `next_track`, so the core pointer can lag the live track and
    /// the now-playing card goes stale. This moves the pointer to the track
    /// with `id` and returns it plus whether the pointer moved; a queue
    /// update is emitted only when it did. Frontend-agnostic — the playback
    /// poll loop calls this to keep now-playing in sync (ADR-006).
    pub async fn sync_current_to_id(&self, id: u64) -> Option<(QueueTrack, bool)> {
        let queue = self.queue.write().await;
        let result = queue.sync_current_to_id(id);
        if matches!(result, Some((_, true))) {
            self.emit(CoreEvent::QueueUpdated {
                state: queue.get_state(),
            })
            .await;
        }
        result
    }

    /// Reconcile an observed engine handover without guessing a duplicate occurrence.
    pub async fn sync_gapless_successor(&self, id: u64) -> Option<(QueueTrack, bool)> {
        let queue = self.queue.write().await;
        let result = queue.sync_gapless_successor(id);
        if matches!(result, Some((_, true))) {
            self.emit(CoreEvent::QueueUpdated {
                state: queue.get_state(),
            })
            .await;
        }
        result
    }

    /// Patch the cached quality of any queued Plex track whose `rating_key`
    /// matches one of `updates` (`(rating_key, bit_depth, sample_rate_khz)`).
    /// Frontend-agnostic hook for the Plex quality-hydration path: a track
    /// hydrated while it is already enqueued/playing carries a frozen quality
    /// snapshot, so this upgrades it in place. Returns true if the CURRENT
    /// track was patched — the caller then re-pushes the now-playing stamp.
    pub async fn patch_plex_queue_quality(
        &self,
        updates: &[(String, Option<u32>, Option<f64>)],
    ) -> bool {
        let queue = self.queue.write().await;
        let current_patched = queue.patch_plex_quality(updates);
        if current_patched {
            self.emit(CoreEvent::QueueUpdated {
                state: queue.get_state(),
            })
            .await;
        }
        current_patched
    }

    /// Attach a resolved cover to any queued track in `ids` that has none.
    /// Frontend-agnostic hook for art that resolves AFTER the row was
    /// enqueued — a disc's cover is fetched, and the fetch outlives the click
    /// that started playback. Returns true if the CURRENT track was patched;
    /// the caller then re-pushes the now-playing stamp.
    pub async fn patch_queue_artwork(&self, ids: &[u64], url: &str) -> bool {
        let queue = self.queue.write().await;
        let (any, current_patched) = queue.patch_artwork(ids, url);
        if any {
            self.emit(CoreEvent::QueueUpdated {
                state: queue.get_state(),
            })
            .await;
        }
        current_patched
    }

    // ==================== Search & Catalog ====================

    /// Search for albums
    pub async fn search_albums(
        &self,
        query: &str,
        limit: u32,
        offset: u32,
        search_type: Option<&str>,
    ) -> Result<SearchResultsPage<Album>, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;

        client
            .search_albums(query, limit, offset, search_type)
            .await
            .map_err(CoreError::Api)
    }

    /// Search for tracks
    pub async fn search_tracks(
        &self,
        query: &str,
        limit: u32,
        offset: u32,
        search_type: Option<&str>,
    ) -> Result<SearchResultsPage<Track>, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;

        client
            .search_tracks(query, limit, offset, search_type)
            .await
            .map_err(CoreError::Api)
    }

    /// Search for artists
    pub async fn search_artists(
        &self,
        query: &str,
        limit: u32,
        offset: u32,
        search_type: Option<&str>,
    ) -> Result<SearchResultsPage<Artist>, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;

        client
            .search_artists(query, limit, offset, search_type)
            .await
            .map_err(CoreError::Api)
    }

    /// Catalog search (combined: albums, tracks, artists, playlists, most_popular).
    /// Returns raw JSON for the caller to parse.
    pub async fn catalog_search(
        &self,
        query: &str,
        limit: u32,
        offset: u32,
    ) -> Result<serde_json::Value, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;

        client
            .catalog_search(query, limit, offset)
            .await
            .map_err(CoreError::Api)
    }

    /// Combined search: `catalog_search` plus parsing of the four category
    /// pages and the `most_popular` hero, with blacklist filtering applied.
    /// The blacklist is a parameter so Search does not depend on the
    /// un-migrated `artist_blacklist` module.
    pub async fn search_all(
        &self,
        query: &str,
        blacklist: &BlacklistFilter,
        album_blacklist: &AlbumBlacklistFilter,
    ) -> Result<SearchAllResults, CoreError> {
        let json = self.catalog_search(query, 30, 0).await?;
        Ok(parse_search_all(&json, blacklist, album_blacklist))
    }

    /// Get album by ID
    pub async fn get_album(&self, album_id: &str) -> Result<Album, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;

        client.get_album(album_id).await.map_err(CoreError::Api)
    }

    /// Get track by ID
    pub async fn get_track(&self, track_id: u64) -> Result<Track, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;

        client.get_track(track_id).await.map_err(CoreError::Api)
    }

    /// Get a track's lyrics document (synced/unsynced), or `None` if the track
    /// has none. Thin wrapper over `QobuzClient::get_lyrics` so headless callers
    /// (qbzd) avoid the `client()` escape hatch. Original-only fetch (no
    /// translation target — the headless surface has no translation toggle).
    pub async fn get_lyrics(
        &self,
        track_id: u64,
    ) -> Result<Option<qbz_qobuz::lyrics::QobuzLyricsDocument>, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;

        client.get_lyrics(track_id, None).await.map_err(CoreError::Api)
    }

    /// Get artist by ID
    pub async fn get_artist(&self, artist_id: u64) -> Result<Artist, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;

        client
            .get_artist_basic(artist_id)
            .await
            .map_err(CoreError::Api)
    }

    // ==================== Streaming ====================

    /// Get stream URL for a track with quality fallback
    pub async fn get_stream_url(
        &self,
        track_id: u64,
        quality: Quality,
    ) -> Result<StreamUrl, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;

        client
            .get_stream_url_with_fallback(track_id, quality)
            .await
            .map_err(CoreError::Api)
    }

    // ==================== Playback Operations ====================

    /// Pause playback
    pub fn pause(&self) -> Result<(), CoreError> {
        self.player.pause().map_err(|e| CoreError::Playback(e))
    }

    /// Resume playback
    pub fn resume(&self) -> Result<(), CoreError> {
        self.player.resume().map_err(|e| CoreError::Playback(e))
    }

    /// Stop playback
    pub fn stop(&self) -> Result<(), CoreError> {
        self.player.stop().map_err(|e| CoreError::Playback(e))
    }

    /// Seek to position in seconds
    pub fn seek(&self, position: u64) -> Result<(), CoreError> {
        self.player
            .seek(position)
            .map_err(|e| CoreError::Playback(e))
    }

    /// Set volume (0.0 - 1.0)
    pub fn set_volume(&self, volume: f32) -> Result<(), CoreError> {
        self.player
            .set_volume(volume)
            .map_err(|e| CoreError::Playback(e))
    }

    /// Get current playback state
    pub fn get_playback_state(&self) -> PlaybackState {
        let state = &self.player.state;
        PlaybackState {
            is_playing: state.is_playing(),
            position: state.current_position(),
            duration: state.duration(),
            track_id: state.current_track_id(),
            volume: state.volume(),
        }
    }

    /// Get the player (for advanced usage)
    pub fn player(&self) -> Arc<Player> {
        Arc::clone(&self.player)
    }

    // ==================== Favorites ====================

    /// Get favorites (albums, tracks, or artists)
    pub async fn get_favorites(
        &self,
        fav_type: &str,
        limit: u32,
        offset: u32,
    ) -> Result<serde_json::Value, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;

        client
            .get_favorites(fav_type, limit, offset)
            .await
            .map_err(CoreError::Api)
    }

    /// Add item to favorites
    pub async fn add_favorite(&self, fav_type: &str, item_id: &str) -> Result<(), CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;

        client
            .add_favorite(fav_type, item_id)
            .await
            .map_err(CoreError::Api)
    }

    /// Remove item from favorites
    pub async fn remove_favorite(&self, fav_type: &str, item_id: &str) -> Result<(), CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;

        client
            .remove_favorite(fav_type, item_id)
            .await
            .map_err(CoreError::Api)
    }

    /// Fetch the set of the user's favorite track IDs. Pages through the
    /// favorites endpoint until exhausted. Used by clients that need to
    /// reflect favorite state on individual tracks (e.g. the Queue
    /// sidebar's now-playing heart).
    pub async fn favorite_track_ids(&self) -> Result<std::collections::HashSet<u64>, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;

        let mut ids = std::collections::HashSet::new();
        let page_size: u32 = 500;
        let mut offset: u32 = 0;
        loop {
            let value = client
                .get_favorites("tracks", page_size, offset)
                .await
                .map_err(CoreError::Api)?;
            let items = value
                .get("tracks")
                .and_then(|t| t.get("items"))
                .and_then(|i| i.as_array())
                .cloned()
                .unwrap_or_default();
            let count = items.len() as u32;
            for item in &items {
                if let Some(id) = item.get("id").and_then(|v| v.as_u64()) {
                    ids.insert(id);
                }
            }
            if count < page_size {
                break;
            }
            offset += page_size;
        }
        Ok(ids)
    }

    /// Fetch the set of the user's favorite (followed) artist IDs. Pages
    /// through the favorites endpoint until exhausted. Used to reflect
    /// follow state on artist cards.
    pub async fn favorite_artist_ids(&self) -> Result<std::collections::HashSet<u64>, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;

        let mut ids = std::collections::HashSet::new();
        let page_size: u32 = 500;
        let mut offset: u32 = 0;
        loop {
            let value = client
                .get_favorites("artists", page_size, offset)
                .await
                .map_err(CoreError::Api)?;
            let items = value
                .get("artists")
                .and_then(|a| a.get("items"))
                .and_then(|i| i.as_array())
                .cloned()
                .unwrap_or_default();
            let count = items.len() as u32;
            for item in &items {
                if let Some(id) = item.get("id").and_then(|v| v.as_u64()) {
                    ids.insert(id);
                }
            }
            if count < page_size {
                break;
            }
            offset += page_size;
        }
        Ok(ids)
    }

    /// Toggle the favorite state of a track. `make_favorite = true` adds it,
    /// `false` removes it. Thin convenience over `add_favorite` /
    /// `remove_favorite` so callers do not duplicate the type string.
    pub async fn set_track_favorite(
        &self,
        track_id: u64,
        make_favorite: bool,
    ) -> Result<(), CoreError> {
        let id = track_id.to_string();
        if make_favorite {
            self.add_favorite("track", &id).await
        } else {
            self.remove_favorite("track", &id).await
        }
    }

    // ==================== Playlists ====================

    /// Get user playlists
    pub async fn get_user_playlists(&self) -> Result<Vec<Playlist>, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;

        client.get_user_playlists().await.map_err(CoreError::Api)
    }

    /// Get playlist by ID
    pub async fn get_playlist(&self, playlist_id: u64) -> Result<Playlist, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;

        client
            .get_playlist(playlist_id)
            .await
            .map_err(CoreError::Api)
    }

    /// Add tracks to playlist
    pub async fn add_tracks_to_playlist(
        &self,
        playlist_id: u64,
        track_ids: &[u64],
    ) -> Result<(), CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;

        client
            .add_tracks_to_playlist(playlist_id, track_ids)
            .await
            .map_err(CoreError::Api)
    }

    /// Lightweight membership fetch: one playlist's track IDS only
    /// (`playlist/get?extra=track_ids`). The membership hydrator's probe —
    /// no Track objects, no pagination.
    pub async fn get_playlist_track_ids(
        &self,
        playlist_id: u64,
    ) -> Result<qbz_models::PlaylistWithTrackIds, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;

        client
            .get_playlist_track_ids(playlist_id)
            .await
            .map_err(CoreError::Api)
    }

    /// Check how many of `track_ids` are already in the Qobuz playlist
    /// `playlist_id`. Mirrors Tauri's `v2_check_playlist_duplicates`
    /// (commands_v2/playlists.rs): fetch the playlist's existing track ids and
    /// set-intersect with the input. This is Qobuz-tracks-into-Qobuz-playlist
    /// only — callers gate out local / Plex / local-playlist targets before
    /// calling (those never duplicate-check, mirroring the Tauri flow).
    pub async fn check_playlist_duplicates(
        &self,
        playlist_id: u64,
        track_ids: &[u64],
    ) -> Result<PlaylistDuplicateResult, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;

        let playlist = client
            .get_playlist_track_ids(playlist_id)
            .await
            .map_err(CoreError::Api)?;
        Ok(compute_playlist_duplicates(&playlist.track_ids, track_ids))
    }

    /// Remove tracks from playlist
    pub async fn remove_tracks_from_playlist(
        &self,
        playlist_id: u64,
        playlist_track_ids: &[u64],
    ) -> Result<(), CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;

        client
            .remove_tracks_from_playlist(playlist_id, playlist_track_ids)
            .await
            .map_err(CoreError::Api)
    }

    /// Move tracks within a playlist (the replacement flow's position step).
    ///
    /// Takes MEMBERSHIP ids, like [`Self::remove_tracks_from_playlist`].
    /// `insert_before` is the 0-based slot to land in front of. Callers must
    /// treat a failure as non-fatal: position is a nicety, and the flows that
    /// use this have already made a write that must not be rolled back.
    pub async fn update_playlist_tracks_position(
        &self,
        playlist_id: u64,
        playlist_track_ids: &[u64],
        insert_before: u32,
    ) -> Result<(), CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;

        client
            .update_playlist_tracks_position(playlist_id, playlist_track_ids, insert_before)
            .await
            .map_err(CoreError::Api)
    }

    /// Create a new playlist
    pub async fn create_playlist(
        &self,
        name: &str,
        description: Option<&str>,
        is_public: bool,
    ) -> Result<Playlist, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;

        client
            .create_playlist(name, description, is_public)
            .await
            .map_err(CoreError::Api)
    }

    /// Follow (subscribe to) a Qobuz playlist so it appears in the user's Qobuz
    /// account across every Qobuz client (and in their user-playlists list).
    pub async fn subscribe_playlist(&self, playlist_id: u64) -> Result<(), CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;

        client
            .subscribe_playlist(playlist_id)
            .await
            .map_err(CoreError::Api)
    }

    /// Unfollow (unsubscribe from) a Qobuz playlist.
    pub async fn unsubscribe_playlist(&self, playlist_id: u64) -> Result<(), CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;

        client
            .unsubscribe_playlist(playlist_id)
            .await
            .map_err(CoreError::Api)
    }

    /// Delete a playlist
    pub async fn delete_playlist(&self, playlist_id: u64) -> Result<(), CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;

        client
            .delete_playlist(playlist_id)
            .await
            .map_err(CoreError::Api)
    }

    /// Update a playlist
    pub async fn update_playlist(
        &self,
        playlist_id: u64,
        name: Option<&str>,
        description: Option<&str>,
        is_public: Option<bool>,
    ) -> Result<Playlist, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;

        client
            .update_playlist(playlist_id, name, description, is_public)
            .await
            .map_err(CoreError::Api)
    }

    /// Search playlists
    pub async fn search_playlists(
        &self,
        query: &str,
        limit: u32,
        offset: u32,
    ) -> Result<SearchResultsPage<Playlist>, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;

        client
            .search_playlists(query, limit, offset)
            .await
            .map_err(CoreError::Api)
    }

    /// Get tracks batch by IDs
    pub async fn get_tracks_batch(&self, track_ids: &[u64]) -> Result<Vec<Track>, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;

        client
            .get_tracks_batch(track_ids)
            .await
            .map_err(CoreError::Api)
    }

    /// Get genres
    pub async fn get_genres(&self, parent_id: Option<u64>) -> Result<Vec<GenreInfo>, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;

        client.get_genres(parent_id).await.map_err(CoreError::Api)
    }

    /// Get discover index
    pub async fn get_discover_index(
        &self,
        genre_ids: Option<Vec<u64>>,
    ) -> Result<DiscoverResponse, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;

        client
            .get_discover_index(genre_ids)
            .await
            .map_err(CoreError::Api)
    }

    /// Get discover playlists
    pub async fn get_discover_playlists(
        &self,
        tag: Option<String>,
        genre_ids: Option<Vec<u64>>,
        limit: Option<u32>,
        offset: Option<u32>,
    ) -> Result<DiscoverPlaylistsResponse, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;

        client
            .get_discover_playlists(tag, genre_ids, limit, offset)
            .await
            .map_err(CoreError::Api)
    }

    /// Get playlist tags
    pub async fn get_playlist_tags(&self) -> Result<Vec<PlaylistTag>, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;

        client.get_playlist_tags().await.map_err(CoreError::Api)
    }

    /// Get discover albums from a specific browse endpoint
    pub async fn get_discover_albums(
        &self,
        endpoint: &str,
        genre_ids: Option<Vec<u64>>,
        offset: u32,
        limit: u32,
    ) -> Result<DiscoverData<DiscoverAlbum>, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;

        client
            .get_discover_albums(endpoint, genre_ids, offset, limit)
            .await
            .map_err(CoreError::Api)
    }

    /// Get featured albums
    pub async fn get_featured_albums(
        &self,
        featured_type: &str,
        limit: u32,
        offset: u32,
        genre_id: Option<u64>,
    ) -> Result<SearchResultsPage<Album>, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;

        client
            .get_featured_albums(featured_type, limit, offset, genre_id)
            .await
            .map_err(CoreError::Api)
    }

    /// Get Release Watch — new releases from followed artists/labels/awards.
    /// `release_type` must be one of "artists" | "labels" | "awards".
    pub async fn get_release_watch(
        &self,
        release_type: &str,
        limit: u32,
        offset: u32,
    ) -> Result<SearchResultsPage<Album>, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;

        client
            .get_release_watch(release_type, limit, offset)
            .await
            .map_err(CoreError::Api)
    }

    /// Get artist page (full artist details with albums, tracks, similar)
    pub async fn get_artist_page(
        &self,
        artist_id: u64,
        sort: Option<&str>,
    ) -> Result<PageArtistResponse, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;

        client
            .get_artist_page(artist_id, sort)
            .await
            .map_err(CoreError::Api)
    }

    /// Get similar artists
    pub async fn get_similar_artists(
        &self,
        artist_id: u64,
        limit: u32,
        offset: u32,
    ) -> Result<SearchResultsPage<Artist>, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;

        client
            .get_similar_artists(artist_id, limit, offset)
            .await
            .map_err(CoreError::Api)
    }

    /// Albums similar to a seed album (`/album/suggest`).
    pub async fn get_album_suggest(
        &self,
        album_id: &str,
    ) -> Result<qbz_models::AlbumSuggestResponse, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;
        client
            .get_album_suggest(album_id)
            .await
            .map_err(CoreError::Api)
    }

    /// Qobuz artist radio (`/radio/artist`) — a generated track list.
    pub async fn get_radio_artist(
        &self,
        artist_id: &str,
    ) -> Result<qbz_models::RadioResponse, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;
        client
            .get_radio_artist(artist_id)
            .await
            .map_err(CoreError::Api)
    }

    /// Qobuz album radio (`/radio/album`).
    pub async fn get_radio_album(
        &self,
        album_id: &str,
    ) -> Result<qbz_models::RadioResponse, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;
        client
            .get_radio_album(album_id)
            .await
            .map_err(CoreError::Api)
    }

    /// Qobuz track radio (`/radio/track`).
    pub async fn get_radio_track(
        &self,
        track_id: &str,
    ) -> Result<qbz_models::RadioResponse, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;
        client
            .get_radio_track(track_id)
            .await
            .map_err(CoreError::Api)
    }

    /// Dynamic mix suggestions (`/dynamic/suggest`) seeded from
    /// recently-listened track ids. Returns the suggested tracks.
    pub async fn get_dynamic_suggest(
        &self,
        listened_track_ids: &[u64],
        limit: u32,
    ) -> Result<Vec<Track>, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;
        client
            .get_dynamic_suggest(listened_track_ids, limit)
            .await
            .map_err(CoreError::Api)
    }

    /// Dynamic mix suggestions with the `track_to_analysed` payload — the
    /// PRIMARY DailyQ/WeeklyQ path (see `QobuzClient::get_dynamic_suggest_full`).
    pub async fn get_dynamic_suggest_full(
        &self,
        listened_track_ids: &[u64],
        tracks_to_analyse: &[TrackToAnalyse],
        limit: u32,
    ) -> Result<Vec<Track>, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;
        client
            .get_dynamic_suggest_full(listened_track_ids, tracks_to_analyse, limit)
            .await
            .map_err(CoreError::Api)
    }

    /// Smart artist radio via the local `qbz-radio` pool builder — a
    /// richer alternative to the Qobuz `/radio/artist` endpoint. Builds
    /// a session pool (seed tracks + similar artists + second-degree)
    /// in the local radio DB, pulls up to 50 track ids from the engine,
    /// and resolves them to full tracks.
    ///
    /// The radio DB uses a `!Send` rusqlite connection, so the pool
    /// build + pull run on blocking threads (mirroring the Tauri
    /// command); the async builder is driven there via `block_on`.
    pub async fn create_smart_artist_radio(
        &self,
        artist_id: u64,
    ) -> Result<Vec<Track>, CoreError> {
        let client = {
            let guard = self.client.read().await;
            guard.as_ref().ok_or(CoreError::NotInitialized)?.clone()
        };

        let build_client = client.clone();
        let session_id = tokio::task::spawn_blocking(move || -> Result<String, String> {
            let db = qbz_radio::RadioDb::open_default()?;
            let builder = qbz_radio::RadioPoolBuilder::new(
                &db,
                &build_client,
                qbz_radio::BuildRadioOptions::default(),
            );
            let session = tokio::runtime::Handle::current()
                .block_on(builder.create_artist_radio(artist_id))?;
            Ok(session.id)
        })
        .await
        .map_err(|e| CoreError::Internal(format!("radio build task: {e}")))?
        .map_err(CoreError::Internal)?;

        let ids = tokio::task::spawn_blocking(move || -> Result<Vec<u64>, String> {
            let db = qbz_radio::RadioDb::open_default()?;
            let engine = qbz_radio::RadioEngine::new(db);
            let mut ids = Vec::new();
            for _ in 0..60 {
                match engine.next_track(&session_id) {
                    Ok(track) => ids.push(track.track_id),
                    Err(_) => break,
                }
            }
            Ok(ids.into_iter().take(50).collect())
        })
        .await
        .map_err(|e| CoreError::Internal(format!("radio pull task: {e}")))?
        .map_err(CoreError::Internal)?;

        let mut tracks = Vec::new();
        for id in ids {
            if let Ok(track) = client.get_track(id).await {
                tracks.push(track);
            }
        }
        Ok(tracks)
    }

    /// Smart Song Radio seeded by a single track via the local `qbz-radio`
    /// pool builder — the immersive Suggestions panel's "Song Radio" card.
    ///
    /// Mirrors `create_smart_artist_radio` (same `!Send` RadioDb/RadioEngine
    /// orchestration via `spawn_blocking` + `Handle::current().block_on`) but
    /// seeds with `create_track_radio(track_id, artist_id)` and HOISTS the
    /// seed track to index 0 (mirroring the Tauri `v2_create_track_radio`
    /// hoist) so the radio always starts on the song the user was playing.
    /// Pulls up to 60 ids, hoists the seed, takes 50, resolves to full tracks.
    ///
    /// Frontend-agnostic (no `tauri::State`, no blacklist filter — the
    /// blacklist lives in the UI/integration layer; callers that need it drop
    /// blacklisted rows after this returns). `seed_track_name` is accepted for
    /// call-site parity with the Tauri command (the local builder seeds by id,
    /// so the name is not needed here) and currently unused.
    pub async fn create_smart_track_radio(
        &self,
        track_id: u64,
        artist_id: u64,
        _seed_track_name: String,
    ) -> Result<Vec<Track>, CoreError> {
        let client = {
            let guard = self.client.read().await;
            guard.as_ref().ok_or(CoreError::NotInitialized)?.clone()
        };

        let build_client = client.clone();
        let session_id = tokio::task::spawn_blocking(move || -> Result<String, String> {
            let db = qbz_radio::RadioDb::open_default()?;
            let builder = qbz_radio::RadioPoolBuilder::new(
                &db,
                &build_client,
                qbz_radio::BuildRadioOptions::default(),
            );
            let session = tokio::runtime::Handle::current()
                .block_on(builder.create_track_radio(track_id, artist_id))?;
            Ok(session.id)
        })
        .await
        .map_err(|e| CoreError::Internal(format!("song radio build task: {e}")))?
        .map_err(CoreError::Internal)?;

        let seed_track_id = track_id;
        let ids = tokio::task::spawn_blocking(move || -> Result<Vec<u64>, String> {
            let db = qbz_radio::RadioDb::open_default()?;
            let engine = qbz_radio::RadioEngine::new(db);
            let mut pulled: Vec<(u64, String)> = Vec::new();
            for _ in 0..60 {
                match engine.next_track(&session_id) {
                    Ok(track) => pulled.push((track.track_id, track.source.clone())),
                    Err(_) => break,
                }
            }
            // Hoist the seed track to the front (mirror of link_resolver.rs).
            if let Some(seed_idx) = pulled
                .iter()
                .position(|(id, source)| *id == seed_track_id && source == "seed_track")
            {
                if seed_idx != 0 {
                    pulled.swap(0, seed_idx);
                }
            }
            Ok(pulled.into_iter().take(50).map(|(id, _)| id).collect())
        })
        .await
        .map_err(|e| CoreError::Internal(format!("song radio pull task: {e}")))?
        .map_err(CoreError::Internal)?;

        let mut tracks = Vec::new();
        for id in ids {
            if let Ok(track) = client.get_track(id).await {
                tracks.push(track);
            }
        }
        Ok(tracks)
    }

    /// Get artist with albums (for album pagination)
    pub async fn get_artist_with_albums(
        &self,
        artist_id: u64,
        limit: Option<u32>,
        offset: Option<u32>,
    ) -> Result<Artist, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;

        client
            .get_artist_with_pagination(artist_id, true, limit, offset)
            .await
            .map_err(CoreError::Api)
    }

    /// Get an artist's albums collection (paginated `ArtistAlbums` only).
    ///
    /// Equivalent to `get_artist_with_albums` but projects only the `albums`
    /// field for callers that don't need the full artist envelope.
    pub async fn get_artist_albums(
        &self,
        artist_id: u64,
        limit: Option<u32>,
        offset: Option<u32>,
    ) -> Result<ArtistAlbums, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;

        let artist = client
            .get_artist_with_pagination(artist_id, true, limit, offset)
            .await
            .map_err(CoreError::Api)?;

        artist
            .albums
            .ok_or_else(|| CoreError::Api(qbz_qobuz::ApiError::ApiResponse(
                "No albums in artist response".to_string(),
            )))
    }

    /// Get artist detail with albums, playlists and appears-on tracks.
    ///
    /// Backs the suggestions panel: requests `extra=albums,tracks_appears_on,playlists`
    /// from `/artist/get` so callers can read `playlists` and `tracks_appears_on`
    /// without a second round-trip.
    pub async fn get_artist_detail(
        &self,
        artist_id: u64,
        limit: Option<u32>,
        offset: Option<u32>,
    ) -> Result<Artist, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;

        client
            .get_artist_detail(artist_id, limit, offset)
            .await
            .map_err(CoreError::Api)
    }

    /// Get an artist's popular/top tracks (`/artist/get?extra=tracks`).
    pub async fn get_artist_tracks(
        &self,
        artist_id: u64,
        limit: u32,
        offset: u32,
    ) -> Result<TracksContainer, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;

        client
            .get_artist_tracks(artist_id, limit, offset)
            .await
            .map_err(CoreError::Api)
    }

    /// Get an artist's releases grid (paginated by `release_type`).
    pub async fn get_releases_grid(
        &self,
        artist_id: u64,
        release_type: &str,
        limit: u32,
        offset: u32,
        sort: Option<&str>,
    ) -> Result<ReleasesGridResponse, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;

        client
            .get_releases_grid(artist_id, release_type, limit, offset, sort)
            .await
            .map_err(CoreError::Api)
    }

    /// Get an artist's Magazine stories (editorial articles). Web client: offset=0 limit=2.
    pub async fn get_artist_story(
        &self,
        artist_id: u64,
        offset: u32,
        limit: u32,
    ) -> Result<ArtistStoryResponse, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;

        client
            .get_artist_story(artist_id, offset, limit)
            .await
            .map_err(CoreError::Api)
    }

    /// Get label page (aggregated: top tracks, releases, playlists, artists)
    pub async fn get_label_page(&self, label_id: u64) -> Result<LabelPageData, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;

        client
            .get_label_page(label_id)
            .await
            .map_err(CoreError::Api)
    }

    /// Enumerate award catalog (/award/explore).
    pub async fn get_award_explore(
        &self,
        limit: u32,
        offset: u32,
    ) -> Result<serde_json::Value, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;
        client
            .get_award_explore(limit, offset)
            .await
            .map_err(CoreError::Api)
    }

    /// Get award page — hero info + award-winning releases.
    pub async fn get_award_page(
        &self,
        award_id: &str,
    ) -> Result<qbz_models::AwardPageData, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;
        client
            .get_award_page(award_id)
            .await
            .map_err(CoreError::Api)
    }

    /// Get paginated albums for an award (/award/getAlbums).
    pub async fn get_award_albums(
        &self,
        award_id: &str,
        limit: u32,
        offset: u32,
    ) -> Result<SearchResultsPage<Album>, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;
        client
            .get_award_albums(award_id, limit, offset)
            .await
            .map_err(CoreError::Api)
    }

    /// Get label explore (discover more labels)
    pub async fn get_label_explore(
        &self,
        limit: u32,
        offset: u32,
    ) -> Result<LabelExploreResponse, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;

        client
            .get_label_explore(limit, offset)
            .await
            .map_err(CoreError::Api)
    }

    /// Get a label's album catalog (paginated, replaces legacy /label/get).
    #[allow(clippy::too_many_arguments)]
    pub async fn get_label_albums(
        &self,
        label_id: u64,
        limit: u32,
        offset: u32,
        sort: Option<String>,
        order: Option<String>,
        genre_ids: Option<String>,
        from_date: Option<String>,
        to_date: Option<String>,
    ) -> Result<LabelListPage<Album>, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;
        client
            .get_label_albums(
                label_id,
                limit,
                offset,
                sort.as_deref(),
                order.as_deref(),
                genre_ids.as_deref(),
                from_date.as_deref(),
                to_date.as_deref(),
            )
            .await
            .map_err(CoreError::Api)
    }

    /// Get a label's upcoming releases.
    pub async fn get_label_next_releases(
        &self,
        label_id: u64,
        limit: u32,
        offset: u32,
        genre_ids: Option<String>,
    ) -> Result<LabelListPage<Album>, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;
        client
            .get_label_next_releases(label_id, limit, offset, genre_ids.as_deref())
            .await
            .map_err(CoreError::Api)
    }

    /// Get a label's press-awarded releases.
    pub async fn get_label_awarded_releases(
        &self,
        label_id: u64,
        limit: u32,
        offset: u32,
        sort: Option<String>,
        order: Option<String>,
        genre_ids: Option<String>,
    ) -> Result<LabelListPage<Album>, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;
        client
            .get_label_awarded_releases(
                label_id,
                limit,
                offset,
                sort.as_deref(),
                order.as_deref(),
                genre_ids.as_deref(),
            )
            .await
            .map_err(CoreError::Api)
    }

    /// Get a label's curated playlists.
    pub async fn get_label_playlists(
        &self,
        label_id: u64,
        limit: u32,
        offset: u32,
    ) -> Result<LabelListPage<Playlist>, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;
        client
            .get_label_playlists(label_id, limit, offset)
            .await
            .map_err(CoreError::Api)
    }

    /// Get a label's top artists.
    pub async fn get_label_top_artists(
        &self,
        label_id: u64,
        limit: u32,
        offset: u32,
    ) -> Result<LabelListPage<Artist>, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;
        client
            .get_label_top_artists(label_id, limit, offset)
            .await
            .map_err(CoreError::Api)
    }

    /// Get a label's editorial story.
    pub async fn get_label_story(
        &self,
        label_id: u64,
        limit: u32,
        offset: u32,
    ) -> Result<LabelStoryResponse, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;
        client
            .get_label_story(label_id, limit, offset)
            .await
            .map_err(CoreError::Api)
    }

    /// Bulk hydrate labels by ID list.
    pub async fn get_label_list(
        &self,
        label_ids: Vec<u64>,
    ) -> Result<LabelGetListResponse, CoreError> {
        let client = self.client.read().await;
        let client = client.as_ref().ok_or(CoreError::NotInitialized)?;
        client
            .get_label_list(&label_ids)
            .await
            .map_err(CoreError::Api)
    }

    // ==================== Event Emission ====================

    /// Emit an event to the frontend adapter
    async fn emit(&self, event: CoreEvent) {
        self.adapter.on_event(event).await;
    }

    /// Get the frontend adapter (for external event emission)
    pub fn adapter(&self) -> Arc<A> {
        Arc::clone(&self.adapter)
    }

    /// Get the Qobuz client (for advanced usage)
    pub fn client(&self) -> Arc<RwLock<Option<QobuzClient>>> {
        Arc::clone(&self.client)
    }

    /// Get the queue manager (for advanced usage)
    pub fn queue(&self) -> Arc<RwLock<QueueManager>> {
        Arc::clone(&self.queue)
    }

    // ----- MusicBrainz -------------------------------------------------------

    /// Whether MusicBrainz integration is currently enabled.
    pub async fn musicbrainz_is_enabled(&self) -> bool {
        self.musicbrainz.is_enabled().await
    }

    /// Enable or disable MusicBrainz integration.
    pub async fn musicbrainz_set_enabled(&self, enabled: bool) {
        self.musicbrainz.set_enabled(enabled).await;
    }

    /// The core's shared MusicBrainz client — the ONE instance whose enabled
    /// flag `musicbrainz_set_enabled` toggles. Frontend code that needs a
    /// client must borrow this one: a private `MusicBrainzClient::new()`
    /// starts enabled and silently ignores the user's opt-out.
    pub fn musicbrainz_client(&self) -> Arc<MusicBrainzClient> {
        Arc::clone(&self.musicbrainz)
    }

    /// The authenticated Qobuz catalog territory (ISO 3166-1 alpha-2), the
    /// scope of every MBID → Qobuz-id identity row. `None` before login —
    /// callers then display but persist nothing (an invented constant would
    /// let one territory's duplicate poison another's).
    pub async fn catalog_scope(&self) -> Option<String> {
        let client = self.client.read().await;
        let client = client.as_ref()?;
        client
            .session()
            .await
            .and_then(|s| s.country_code)
            .map(|c| c.trim().to_uppercase())
            .filter(|c| !c.is_empty())
    }

    /// Resolve an artist NAME to a MusicBrainz id under the containment rule
    /// (`identity::select_exact_name`): exactly one result in the quoted-name
    /// window with the same name and score >= 90, else `None`. Used where
    /// only a name exists (playlist suggestions, local artist images). The
    /// search WINDOW is cached as evidence and re-gated on every hit; the
    /// legacy name-keyed `resolved_artists` table is neither read nor written
    /// — a name alone is not an identity (#768).
    pub async fn musicbrainz_resolve_artist(
        &self,
        name: &str,
    ) -> Result<Option<ResolvedArtist>, CoreError> {
        let store = CoreIdentityStore(&self.musicbrainz_cache);
        identity::resolve_artist_by_name(Some(&store), self.musicbrainz.as_ref(), name)
            .await
            .map_err(|e| CoreError::Internal(e.to_string()))
    }

    /// Resolve the SOURCE artist of an Artist Page — a Qobuz artist id, its
    /// name and the ISRCs of its own Popular Tracks — to a MusicBrainz id
    /// (`identity::resolve_source_artist`). Verified mappings are cached by
    /// Qobuz artist id (Qobuz artist ids are catalog-global: territory
    /// decides availability, not which artist an id names), bounded to
    /// `MAX_SOURCE_ISRCS` MusicBrainz lookups through the shared client.
    pub async fn musicbrainz_resolve_source_artist(
        &self,
        qobuz_artist_id: u64,
        name: &str,
        isrcs: &[String],
    ) -> identity::SourceResolution {
        if !self.musicbrainz.is_enabled().await {
            return identity::SourceResolution {
                identity: identity::SourceIdentity::Unavailable,
                resolved: None,
                requests: 0,
                cache: "disabled",
            };
        }
        let store = CoreIdentityStore(&self.musicbrainz_cache);
        identity::resolve_source_artist(
            Some(&store),
            self.musicbrainz.as_ref(),
            qobuz_artist_id,
            name,
            isrcs,
        )
        .await
    }

    /// Generate playlist "Suggested Songs" via the artist_vectors engine.
    /// Resolves each playlist artist NAME to a confident MusicBrainz id, then
    /// runs the SuggestionsEngine over the core-owned clients + the per-user
    /// vector store. The names come from the playlist's own Qobuz tracks (so
    /// they are already canonical — the Tauri command's extra Qobuz-name
    /// re-fetch is unnecessary; `qobuz_id` is accepted for forward use).
    pub async fn generate_playlist_suggestions(
        &self,
        artists: Vec<(Option<u64>, String)>,
        exclude_track_ids: Vec<u64>,
        include_reasons: bool,
        config: Option<qbz_reco::SuggestionConfig>,
    ) -> Result<qbz_reco::SuggestionResult, String> {
        use std::collections::HashSet;

        if artists.is_empty() {
            return Ok(qbz_reco::SuggestionResult {
                tracks: Vec::new(),
                source_artists: Vec::new(),
                playlist_artists_count: 0,
                similar_artists_count: 0,
            });
        }

        // Resolve each artist name to a confident MusicBrainz id, deduped.
        let mut resolved: Vec<(String, String)> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for (_qobuz_id, name) in &artists {
            if let Ok(Some(r)) = self.musicbrainz_resolve_artist(name).await {
                if seen.insert(r.mbid.clone()) {
                    resolved.push((r.mbid, name.clone()));
                }
            }
        }

        if resolved.is_empty() {
            log::warn!("[suggestions] no playlist artists resolved to MusicBrainz ids");
            return Ok(qbz_reco::SuggestionResult {
                tracks: Vec::new(),
                source_artists: Vec::new(),
                playlist_artists_count: artists.len(),
                similar_artists_count: 0,
            });
        }

        let config = config.unwrap_or_default();
        let builder = Arc::new(qbz_reco::ArtistVectorBuilder::new(
            self.artist_vectors.clone(),
            self.musicbrainz.clone(),
            self.musicbrainz_cache.clone(),
            self.client.clone(),
            qbz_reco::RelationshipWeights::default(),
        ));
        let engine = qbz_reco::SuggestionsEngine::new(
            self.artist_vectors.clone(),
            builder,
            self.client.clone(),
            config,
        );

        let exclude: HashSet<u64> = exclude_track_ids.into_iter().collect();
        engine
            .generate_suggestions(&resolved, &exclude, include_reasons)
            .await
    }

    /// Fetch the artist metadata (location, life_span, genre seeds) for
    /// the Origin section of the artist network sidebar. Resolves the
    /// real country from the begin_area hierarchy when a city-level
    /// location is found, because MB's `country` field is where the
    /// artist is active, not where they were born/formed.
    pub async fn musicbrainz_get_artist_metadata(
        &self,
        mbid: &str,
    ) -> Result<ArtistMetadata, CoreError> {
        // Cache lookup — same behavior as Tauri's v2 command.
        if let Ok(guard) = self.musicbrainz_cache.lock() {
            if let Some(cache) = guard.as_ref() {
                if let Ok(Some(cached)) = cache.get_artist_metadata(mbid) {
                    return Ok(cached);
                }
            }
        }

        let artist = self
            .musicbrainz
            .get_artist_with_relations(mbid)
            .await
            .map_err(|e| CoreError::Internal(e.to_string()))?;

        let mut metadata = location::extract_metadata(&artist);

        if let Some(ref mut loc) = metadata.location {
            if loc.city.is_some() {
                if let Some(ref area_id) = loc.area_id {
                    if let Ok(Some((country_name, country_code))) =
                        self.musicbrainz.resolve_area_country(area_id).await
                    {
                        loc.display_name = format!("{}, {}", loc.display_name, country_name);
                        loc.country = Some(country_name);
                        loc.country_code = country_code;
                    }
                }
            }
        }

        if let Ok(guard) = self.musicbrainz_cache.lock() {
            if let Some(cache) = guard.as_ref() {
                let _ = cache.set_artist_metadata(mbid, &metadata);
            }
        }

        Ok(metadata)
    }

    /// Fetch the artist relationships (band members, member-of groups,
    /// collaborators) for the Relationships section of the sidebar.
    /// Splits `member of band` by direction: backward direction lists
    /// members of *this* artist (still-active vs ended -> past), forward
    /// direction lists groups this artist is a member of.
    pub async fn musicbrainz_get_artist_relationships(
        &self,
        mbid: &str,
    ) -> Result<ArtistRelationships, CoreError> {
        if let Ok(guard) = self.musicbrainz_cache.lock() {
            if let Some(cache) = guard.as_ref() {
                if let Ok(Some(cached)) = cache.get_artist_relations(mbid) {
                    return Ok(cached);
                }
            }
        }

        let artist = self
            .musicbrainz
            .get_artist_with_relations(mbid)
            .await
            .map_err(|e| CoreError::Internal(e.to_string()))?;

        let mut members = Vec::new();
        let mut past_members = Vec::new();
        let mut groups = Vec::new();
        let mut collaborators = Vec::new();

        if let Some(relations) = &artist.relations {
            for relation in relations {
                let Some(related_artist) = &relation.artist else {
                    continue;
                };

                let related = RelatedArtist {
                    mbid: related_artist.id.clone(),
                    name: related_artist.name.clone(),
                    role: relation
                        .attributes
                        .as_ref()
                        .and_then(|a| a.first().cloned()),
                    period: Some(Period {
                        begin: relation.begin.clone(),
                        end: relation.end.clone(),
                    }),
                    ended: relation.ended.unwrap_or(false),
                };

                match relation.relation_type.as_str() {
                    "member of band" => {
                        if relation.direction.as_deref() == Some("backward") {
                            if related.ended {
                                past_members.push(related);
                            } else {
                                members.push(related);
                            }
                        } else {
                            groups.push(related);
                        }
                    }
                    "collaboration" => {
                        collaborators.push(related);
                    }
                    _ => {}
                }
            }
        }

        let result = ArtistRelationships {
            members,
            past_members,
            groups,
            collaborators,
        };

        if let Ok(guard) = self.musicbrainz_cache.lock() {
            if let Some(cache) = guard.as_ref() {
                let _ = cache.set_artist_relations(mbid, &result);
            }
        }

        Ok(result)
    }

    /// "You may also like" tag-based discovery — finds artists that
    /// share the seed artist's primary genre tag on MusicBrainz, then
    /// validates exact name matches on Qobuz so the row can actually
    /// open the artist page. Filters out the seed itself, the artists
    /// already shown in the Similar section, and any dismissed
    /// artists (passed in by the caller; the dismiss store lives at
    /// the frontend layer).
    ///
    /// When the primary tag does not return enough validated results,
    /// the pipeline falls back to the secondary tag, dedupes against
    /// the primary's results, and tops up. Result ordering is
    /// deterministic per seed_mbid (same artist page = same shuffle).
    pub async fn musicbrainz_discover_artists(
        &self,
        seed_mbid: &str,
        seed_name: &str,
        similar_names: &[String],
        dismissed_per_tag: &(dyn Fn(&str) -> std::collections::HashSet<String> + Send + Sync),
        known_artists: &(dyn Fn() -> (
            std::collections::HashSet<u64>,
            std::collections::HashSet<String>,
        )
                       + Send
                       + Sync),
    ) -> Result<DiscoveryResponse, CoreError> {
        use std::collections::HashSet;

        if !self.musicbrainz.is_enabled().await {
            return Ok(DiscoveryResponse {
                artists: Vec::new(),
                primary_tag: String::new(),
            });
        }

        let seed_tags = self
            .musicbrainz
            .get_artist_tags(seed_mbid)
            .await
            .unwrap_or_default();
        if seed_tags.is_empty() {
            return Ok(DiscoveryResponse {
                artists: Vec::new(),
                primary_tag: String::new(),
            });
        }

        let primary_tag = seed_tags[0].clone();
        let mb_results = self
            .musicbrainz
            .search_artists_by_tag(&primary_tag, 50)
            .await
            .map_err(|e| CoreError::Internal(e.to_string()))?;

        if mb_results.artists.is_empty() {
            return Ok(DiscoveryResponse {
                artists: Vec::new(),
                primary_tag,
            });
        }

        let seed_norm = normalize_artist_name(seed_name);
        let similar_norm: HashSet<String> =
            similar_names.iter().map(|n| normalize_artist_name(n)).collect();
        let dismissed_primary = dismissed_per_tag(&primary_tag.to_lowercase());
        let (known_ids, known_names) = known_artists();

        let mut candidates: Vec<(String, String)> = Vec::new();
        for artist in &mb_results.artists {
            let n = normalize_artist_name(&artist.name);
            if n == seed_norm
                || artist.id.eq_ignore_ascii_case(seed_mbid)
                || similar_norm.contains(&n)
                || dismissed_primary.contains(&n)
                || known_names.contains(&n)
            {
                continue;
            }
            candidates.push((artist.id.clone(), artist.name.clone()));
        }

        shuffle_with_seed(&mut candidates, seed_mbid, None);

        let max_results = 8;
        let min_results = 5;
        let mut results = self
            .validate_discovery_on_qobuz(&candidates, max_results, &known_ids)
            .await;

        if results.len() < min_results && seed_tags.len() > 1 {
            let secondary_tag = seed_tags[1].clone();
            let dismissed_secondary = dismissed_per_tag(&secondary_tag.to_lowercase());
            let existing_mbids: HashSet<String> =
                results.iter().map(|r| r.mbid.clone()).collect();
            if let Ok(secondary) = self
                .musicbrainz
                .search_artists_by_tag(&secondary_tag, 30)
                .await
            {
                let mut secondary_candidates: Vec<(String, String)> = Vec::new();
                for a in &secondary.artists {
                    let n = normalize_artist_name(&a.name);
                    if n == seed_norm
                        || a.id.eq_ignore_ascii_case(seed_mbid)
                        || similar_norm.contains(&n)
                        || dismissed_primary.contains(&n)
                        || dismissed_secondary.contains(&n)
                        || known_names.contains(&n)
                        || existing_mbids.contains(&a.id)
                    {
                        continue;
                    }
                    secondary_candidates.push((a.id.clone(), a.name.clone()));
                }
                shuffle_with_seed(&mut secondary_candidates, seed_mbid, Some(&secondary_tag));
                let remaining = max_results.saturating_sub(results.len());
                let mut more = self
                    .validate_discovery_on_qobuz(
                        &secondary_candidates,
                        remaining,
                        &known_ids,
                    )
                    .await;
                results.append(&mut more);
            }
        }

        Ok(DiscoveryResponse {
            artists: results,
            primary_tag,
        })
    }

    async fn validate_discovery_on_qobuz(
        &self,
        candidates: &[(String, String)],
        max: usize,
        known_ids: &std::collections::HashSet<u64>,
    ) -> Vec<DiscoveryArtist> {
        let mut out: Vec<DiscoveryArtist> = Vec::new();
        for (mbid, name) in candidates {
            if out.len() >= max {
                break;
            }
            let Ok(page) = self.search_artists(name, 1, 0, None).await else {
                continue;
            };
            let Some(first) = page.items.first() else {
                continue;
            };
            if normalize_artist_name(&first.name) != normalize_artist_name(name) {
                continue;
            }
            // Tauri's `!local_known_qobuz_ids.contains(&artist.id)`
            // gate — never suggest an artist the user has already
            // listened to >2 times.
            if known_ids.contains(&first.id) {
                continue;
            }
            out.push(DiscoveryArtist {
                mbid: mbid.clone(),
                name: first.name.clone(),
                qobuz_id: Some(first.id),
            });
        }
        out
    }
}

// ----- Location / scene discovery -------------------------------------------

/// Which stage of scene discovery a progress event describes.
///
/// [`ScenePhase::as_str`] is the ONLY place the wire spelling lives — the
/// scene bridge document (`progress.phase`) is `"searching" | "validating" |
/// "done"` and nothing else may hand-write those literals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScenePhase {
    Searching,
    Validating,
    Done,
}

impl ScenePhase {
    pub fn as_str(self) -> &'static str {
        match self {
            ScenePhase::Searching => "searching",
            ScenePhase::Validating => "validating",
            ScenePhase::Done => "done",
        }
    }
}

/// Cooperative cancellation for scene discovery.
///
/// A generation guard in the frontend only drops the FINAL publish; it does
/// not stop the traffic behind it, and ONE page can run up to 100 sequential
/// Qobuz artist searches on top of rate-limited MusicBrainz calls (which are
/// serialised at ~1.1 s each). This token is checked between every request, so
/// leaving the view actually stops the work instead of merely ignoring it.
///
/// Deliberately an `AtomicBool` rather than `tokio_util::sync::
/// CancellationToken`: no new dependency, and `Clone` is free into whichever
/// task owns the discovery. The frontend holds one token per generation and
/// trips the old one on a new scene, on Retry, on view deactivation and on an
/// offline transition.
#[derive(Clone, Debug, Default)]
pub struct CancelToken(Arc<std::sync::atomic::AtomicBool>);

impl CancelToken {
    /// A fresh, un-tripped token.
    pub fn new() -> Self {
        Self::default()
    }

    /// Trip the token. Idempotent; every clone sees it.
    pub fn cancel(&self) {
        self.0.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    pub fn is_cancelled(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// Why scene discovery could not produce a page.
///
/// This type exists because the old signature returned `Ok(vec![])` for
/// offline, MusicBrainz-disabled, unauthenticated, rate-limited AND
/// genuinely-empty alike — so the UI's error state was unreachable and all
/// five collapsed into "No matching artists from this scene…". Every variant
/// here is the ERROR class (error state + Retry) except [`Self::Cancelled`],
/// which the user asked for and must paint nothing at all.
///
/// "Empty" is NOT an error: a query that completed and found nothing still
/// returns `Ok` with an empty `artists` vector.
#[derive(Debug, thiserror::Error)]
pub enum SceneDiscoveryError {
    /// No Qobuz client — the core booted offline-tolerant, or never init'd.
    #[error("Not initialized")]
    NotInitialized,

    /// Qobuz refused on authentication/entitlement grounds.
    #[error("Authentication required: {0}")]
    NotAuthenticated(String),

    /// EVERY MusicBrainz genre search failed. Also how a disabled MusicBrainz
    /// integration surfaces, since the client refuses each call.
    #[error("MusicBrainz unavailable: {0}")]
    UpstreamUnavailable(String),

    /// There were candidates to validate and EVERY Qobuz search failed.
    #[error("Qobuz catalog unavailable: {0}")]
    CatalogUnavailable(String),

    /// The caller tripped the [`CancelToken`]. Never user-visible.
    #[error("Scene discovery cancelled")]
    Cancelled,

    #[error("Internal error: {0}")]
    Internal(String),
}

impl SceneDiscoveryError {
    /// True when the view should show its error state with a Retry button.
    /// False only for [`Self::Cancelled`], which must leave the UI untouched.
    pub fn shows_error_state(&self) -> bool {
        !matches!(self, SceneDiscoveryError::Cancelled)
    }

    pub fn is_cancelled(&self) -> bool {
        matches!(self, SceneDiscoveryError::Cancelled)
    }
}

impl From<SceneDiscoveryError> for CoreError {
    fn from(err: SceneDiscoveryError) -> Self {
        match err {
            SceneDiscoveryError::NotInitialized => CoreError::NotInitialized,
            SceneDiscoveryError::NotAuthenticated(msg) => CoreError::AuthFailed(msg),
            other => CoreError::Internal(other.to_string()),
        }
    }
}

/// Does this Qobuz failure end the whole page, or just this one candidate?
///
/// Terminal classes abort the loop immediately instead of burning up to 99
/// more doomed searches: no client, an auth rejection, an entitlement/403
/// refusal, the open 403 circuit breaker, and offline mode. Everything else
/// (timeouts, 5xx, one odd response) is per-candidate and merely counted, so a
/// single flaky artist cannot fail a whole scene.
fn scene_fatal_qobuz(err: &CoreError) -> Option<SceneDiscoveryError> {
    use qbz_qobuz::ApiError;
    match err {
        CoreError::NotInitialized => Some(SceneDiscoveryError::NotInitialized),
        CoreError::AuthRequired => {
            Some(SceneDiscoveryError::NotAuthenticated("session missing".into()))
        }
        CoreError::AuthFailed(msg) => Some(SceneDiscoveryError::NotAuthenticated(msg.clone())),
        CoreError::Api(api) => match api {
            ApiError::AuthenticationError(_)
            | ApiError::InvalidAppId
            | ApiError::InvalidAppSecret
            | ApiError::IneligibleUser
            | ApiError::Forbidden(_)
            | ApiError::ForbiddenCircuitOpen(_) => {
                Some(SceneDiscoveryError::NotAuthenticated(api.to_string()))
            }
            ApiError::OfflineMode => Some(SceneDiscoveryError::CatalogUnavailable(api.to_string())),
            _ => None,
        },
        _ => None,
    }
}

/// Drop blacklisted rows from a scene response and keep `total_candidates`
/// honest. Returns how many were removed.
///
/// Run on EVERY cache hit: a hit short-circuits the validation loop (which is
/// where the blacklist is normally applied, Tauri-style), so without this a
/// blocked artist would keep reappearing for the whole 30-day TTL. Rows with
/// no usable Qobuz id are kept — fail-open, same as the Slint adapter.
fn apply_scene_blacklist(
    response: &mut LocationDiscoveryResponse,
    blacklist: &(dyn Fn(u64) -> bool + Send + Sync),
) -> usize {
    let before = response.artists.len();
    response.artists.retain(|c| match c.qobuz_id {
        Some(id) if id > 0 => !blacklist(id as u64),
        _ => true,
    });
    let removed = before - response.artists.len();
    response.total_candidates = response.total_candidates.saturating_sub(removed);
    if removed > 0 {
        response.has_more = response.next_offset < response.total_candidates;
    }
    removed
}

impl<A: FrontendAdapter + Send + Sync + 'static> QbzCore<A> {
    /// Throw away every cached scene.
    ///
    /// Call it when the artist blacklist (or its enabled flag) changes, and on
    /// account switch. The post-filter on read covers newly-BLOCKED artists;
    /// this covers the two cases it cannot: un-blocking (the validation loop
    /// had already substituted a different same-name Qobuz artist for that
    /// MBID). The account territory is part of the v2 key, so a territory
    /// change no longer needs this.
    pub fn invalidate_scene_cache(&self) {
        if let Ok(guard) = self.musicbrainz_cache.lock() {
            if let Some(cache) = guard.as_ref() {
                if let Err(e) = cache.clear_scene_cache() {
                    log::warn!("invalidate_scene_cache: {e}");
                }
            }
        }
    }

    /// "Artists from the same place" — given a source artist's MBID,
    /// area and genre/tag seeds, find other artists from that area
    /// who share the genres, validated against Qobuz.
    ///
    /// Thin wrapper over [`Self::discover_artists_by_location_with_progress`]
    /// with no progress sink, no blacklist and a token that is never tripped.
    /// It exists because Rust has no default arguments: adding parameters to
    /// this method would break every existing caller, and the Slint binary's
    /// `location_view.rs` (which does its own blacklist pass on the way out)
    /// must keep compiling untouched.
    ///
    /// NOTE the one behavioural change for existing callers: a page where
    /// EVERY upstream search failed is now `Err`, where it used to be `Ok`
    /// with an empty vector. That is the entire point of B7 — offline,
    /// MusicBrainz-off, unauthenticated and "genuinely nobody" are different
    /// answers and the UI cannot say so if the core flattens them.
    #[allow(clippy::too_many_arguments)]
    pub async fn discover_artists_by_location(
        &self,
        source_mbid: &str,
        area_id: Option<&str>,
        area_name: &str,
        country: Option<&str>,
        genres: Vec<String>,
        tags: Vec<String>,
        limit: usize,
        offset: usize,
    ) -> Result<LocationDiscoveryResponse, CoreError> {
        let no_blacklist = |_id: u64| false;
        let no_progress = |_phase: ScenePhase, _pct: u8, _detail: String| {};
        self.discover_artists_by_location_with_progress(
            source_mbid,
            area_id,
            area_name,
            country,
            genres,
            tags,
            limit,
            offset,
            &no_blacklist,
            &CancelToken::new(),
            &no_progress,
        )
        .await
        .map_err(CoreError::from)
    }

    /// Scene discovery with a progress stream, a blacklist predicate and
    /// cancellation. Ports `v2_discover_artists_by_location` end to end —
    /// subdivision resolution, affinity scoring, the 30-day scene cache and
    /// the four progress emit points.
    ///
    /// `blacklist` is INJECTED rather than imported because the blacklist
    /// store lives in `qbz-app`/the frontend and `qbz-core` must not learn
    /// about a frontend module. It is applied in two places, exactly as Tauri
    /// did: inside the validation loop (so a blocked artist lets the
    /// NEXT-best same-name Qobuz artist take the slot rather than losing the
    /// row) and on every cache hit.
    ///
    /// `progress` fires with `(phase, 0..=100, detail)`. Two paths emit
    /// NOTHING and that is deliberate: a cache hit and a no-seed return.
    /// Completion is signalled by this future resolving, never by waiting for
    /// a `Done` event — a cached scene would otherwise hang the bar at 0 %.
    /// The 95 → 100 jump at the end is Tauri's and is kept; the bar's 300 ms
    /// width transition smooths it.
    #[allow(clippy::too_many_arguments)]
    pub async fn discover_artists_by_location_with_progress(
        &self,
        source_mbid: &str,
        area_id: Option<&str>,
        area_name: &str,
        country: Option<&str>,
        genres: Vec<String>,
        tags: Vec<String>,
        limit: usize,
        offset: usize,
        blacklist: &(dyn Fn(u64) -> bool + Send + Sync),
        cancel: &CancelToken,
        progress: &(dyn Fn(ScenePhase, u8, String) + Send + Sync),
    ) -> Result<LocationDiscoveryResponse, SceneDiscoveryError> {
        use std::collections::HashMap;

        if cancel.is_cancelled() {
            return Err(SceneDiscoveryError::Cancelled);
        }

        let source_seeds = AffinitySeeds {
            genres: genres.clone(),
            tags: tags.clone(),
            normalized_seeds: genres.iter().chain(tags.iter()).cloned().collect(),
        };

        // The cache is consulted BEFORE the area resolution below, unlike
        // Tauri: resolution is a pure function of `area_id`, so keying on the
        // pre-resolution identity saves up to five MusicBrainz round-trips on
        // every hit. First page only, matching Tauri — paginated pages are
        // cheap to recompute and would multiply the key space by the offset.
        // The catalog territory is part of the key (v2) — which same-name
        // Qobuz artist validates depends on it. Without a scope (not logged
        // in) the scene is computed but neither read from nor written to the
        // materialised cache, rather than keyed on an invented constant.
        let scope = self.catalog_scope().await;
        let store = CoreIdentityStore(&self.musicbrainz_cache);
        let catalog = SceneQobuz {
            core: self,
            fatal: std::sync::Mutex::new(None),
        };
        let cache_key = scope.as_deref().map(|scope| {
            location::build_scene_cache_key_v1(&location::SceneCacheKey {
                source_mbid,
                area_id: area_id.unwrap_or(area_name),
                country,
                seeds: &source_seeds,
                page_size: limit,
                catalog_scope: Some(scope),
            })
        });
        if cache_key.is_none() {
            log::info!("[scene] no catalog scope (not logged in): scene cache bypassed");
        }

        if let (0, Some(cache_key)) = (offset, cache_key.as_deref()) {
            let mut cached = None;
            if let Ok(guard) = self.musicbrainz_cache.lock() {
                if let Some(cache) = guard.as_ref() {
                    cached = cache.get_scene_cache(cache_key).ok().flatten();
                }
            }
            if let Some(mut hit) = cached {
                let removed = apply_scene_blacklist(&mut hit, blacklist);
                // The materialised scene is NOT an identity authority: every
                // row's Qobuz projection is re-derived from the identifier-
                // keyed identity cache (verified rows live 30 days, a
                // provisional unique-name row 7 days, then one cheap Qobuz
                // re-search). A provisional match therefore never silently
                // becomes a 30-day association because the page was cached.
                let dropped = self
                    .revalidate_scene_rows(&mut hit, &store, &catalog, scope.as_deref(), blacklist)
                    .await?;
                log::info!(
                    "[scene] cache hit for {} ({} artists, {} blacklisted out, {} dropped on revalidation)",
                    area_name,
                    hit.artists.len(),
                    removed,
                    dropped
                );
                return Ok(hit);
            }
        }

        // Step 0: smart area resolution — city → parent subdivision
        // for broader results (Leyton → England, Seattle →
        // Washington). A failure here is PARTIAL, not fatal: the raw
        // area name still searches, just less broadly.
        let (search_name, display_name) = match area_id {
            Some(aid) => match self.musicbrainz.resolve_parent_subdivision(aid).await {
                Ok(Some((subdivision, _))) => {
                    let display = country
                        .map(|c| format!("{}, {}", c, subdivision))
                        .unwrap_or_else(|| subdivision.clone());
                    (subdivision, display)
                }
                other => {
                    if let Err(e) = other {
                        log::warn!("[scene] area resolution failed for {area_name:?}: {e}");
                    }
                    let display = country
                        .map(|c| format!("{}, {}", c, area_name))
                        .unwrap_or_else(|| area_name.to_string());
                    (area_name.to_string(), display)
                }
            },
            None => {
                let display = country
                    .map(|c| format!("{}, {}", c, area_name))
                    .unwrap_or_else(|| area_name.to_string());
                (area_name.to_string(), display)
            }
        };

        if cancel.is_cancelled() {
            return Err(SceneDiscoveryError::Cancelled);
        }

        // Step 2: pick search genres, dropping overly broad tags that
        // would return the whole country's catalog.
        let mut search_genres: Vec<String> = if genres.is_empty() {
            tags.iter()
                .filter(|s| !is_broad_genre(s))
                .take(3)
                .cloned()
                .collect()
        } else {
            genres
                .iter()
                .chain(tags.iter().take(2))
                .filter(|s| !is_broad_genre(s))
                .cloned()
                .collect()
        };
        if search_genres.is_empty() {
            // Everything was broad — fall back to the raw list.
            search_genres = if genres.is_empty() {
                tags.iter().take(3).cloned().collect()
            } else {
                genres.iter().take(3).cloned().collect()
            };
        }
        if search_genres.is_empty() {
            return Ok(LocationDiscoveryResponse {
                artists: Vec::new(),
                scene_label: format!("{} scene", display_name),
                genre_summary: String::new(),
                total_candidates: 0,
                has_more: false,
                next_offset: 0,
            });
        }

        // Step 2/3: MB tag+area search per genre, dedupe + score.
        // candidate_map: mbid -> (name, score_sum, genre_hits, tags)
        let mut candidate_map: HashMap<String, (String, i32, usize, Vec<String>)> =
            HashMap::new();
        let per_genre_limit = 200usize;
        let total_genres = search_genres.len();
        // Failures used to be swallowed one by one and the whole method still
        // returned Ok(empty). Counted now, because "one genre 503'd" and "the
        // integration is off / we are offline" must not look the same.
        let mut genre_failures = 0usize;
        let mut last_genre_error = String::new();
        for (genre_idx, genre) in search_genres.iter().enumerate() {
            if cancel.is_cancelled() {
                return Err(SceneDiscoveryError::Cancelled);
            }
            // Progress: MusicBrainz search phase = 0-40 %.
            progress(
                ScenePhase::Searching,
                ((genre_idx as f64 / total_genres as f64) * 40.0) as u8,
                genre.clone(),
            );

            // The client already retries 503/429 with the server's Retry-After
            // (send_with_retry, up to 3 attempts), so the scene no longer stacks
            // a one-shot backoff on top -- that could pile waits past ~10s.
            let result = self
                .musicbrainz
                .search_artists_by_tag_and_area(genre, &search_name, country, per_genre_limit, 0)
                .await;

            let response = match result {
                Ok(response) => response,
                Err(e) => {
                    log::warn!("[scene] genre+area search failed for {genre:?}: {e}");
                    genre_failures += 1;
                    last_genre_error = e.to_string();
                    continue;
                }
            };
            for artist in &response.artists {
                if artist.id == source_mbid {
                    continue;
                }
                let candidate_tags: Vec<String> = artist
                    .tags
                    .as_ref()
                    .map(|list| {
                        list.iter()
                            .filter(|t| t.count.unwrap_or(0) > 0)
                            .map(|t| t.name.clone())
                            .collect()
                    })
                    .unwrap_or_default();
                let same_city = artist
                    .begin_area
                    .as_ref()
                    .map(|ba| {
                        ba.name.eq_ignore_ascii_case(&search_name)
                            || area_id.map(|aid| ba.id == aid).unwrap_or(false)
                    })
                    .unwrap_or(false);
                let same_country = artist
                    .area
                    .as_ref()
                    .map(|a| a.name.eq_ignore_ascii_case(&search_name))
                    .unwrap_or(false);
                let score =
                    compute_affinity_score(&candidate_tags, &source_seeds, same_city, same_country);
                let entry = candidate_map
                    .entry(artist.id.clone())
                    .or_insert_with(|| (artist.name.clone(), 0, 0, Vec::new()));
                entry.1 += score;
                entry.2 += 1;
                for tag in &candidate_tags {
                    if !entry.3.contains(tag) {
                        entry.3.push(tag.clone());
                    }
                }
            }
        }

        // Every upstream search failed => this is an ERROR, not an empty
        // scene. It is also how "MusicBrainz is disabled" arrives, since the
        // client refuses each call with ServiceUnavailable.
        if total_genres > 0 && genre_failures == total_genres {
            return Err(SceneDiscoveryError::UpstreamUnavailable(last_genre_error));
        }

        // Step 3: score + sort. Final = affinity + (genre_hits-1)*15.
        let mut scored: Vec<(String, String, Vec<String>, i32)> = candidate_map
            .into_iter()
            .map(|(mbid, (name, score, genre_hits, tag_list))| {
                let candidate_seeds = extract_affinity_seeds(
                    &tag_list
                        .iter()
                        .map(|name| Tag {
                            name: name.clone(),
                            count: Some(1),
                        })
                        .collect::<Vec<_>>(),
                );
                let multi_genre_bonus = ((genre_hits as i32) - 1) * 15;
                (mbid, name, candidate_seeds.genres, score + multi_genre_bonus)
            })
            .collect();
        scored.sort_by(|a, b| b.3.cmp(&a.3).then_with(|| a.0.cmp(&b.0)));

        let total_candidates = scored.len();
        let to_validate: Vec<_> = scored.into_iter().skip(offset).take(limit).collect();

        // Step 4: validate against Qobuz (exact normalized-name match,
        // pick the one with the most albums as a popularity proxy).
        let total_to_validate = to_validate.len();
        progress(
            ScenePhase::Validating,
            40,
            format!("{} candidates", total_to_validate),
        );

        let mut validated: Vec<LocationCandidate> = Vec::new();
        let mut validation_failures = 0usize;
        let mut last_validation_error = String::new();
        for (validate_idx, (mbid, mb_name, candidate_genres, score)) in
            to_validate.iter().enumerate()
        {
            if cancel.is_cancelled() {
                return Err(SceneDiscoveryError::Cancelled);
            }
            // Progress: Qobuz validation phase = 40-95 %, every 5th row.
            if validate_idx % 5 == 0 && total_to_validate > 0 {
                progress(
                    ScenePhase::Validating,
                    40 + ((validate_idx as f64 / total_to_validate as f64) * 55.0) as u8,
                    format!("{}/{}", validate_idx, total_to_validate),
                );
            }

            // Identity, not popularity (#768): `identity::resolve_scene_candidate`
            // keys its cache on THIS candidate's MBID + the catalog scope,
            // takes a unique exact-name Qobuz artist as a provisional match,
            // and splits several same-name Qobuz artists with their own
            // top-track ISRCs — never with `albums_count` alone. No evidence,
            // no row.
            match identity::resolve_scene_candidate(
                Some(&store),
                self.musicbrainz.as_ref(),
                &catalog,
                mbid,
                mb_name,
                scope.as_deref(),
                blacklist,
            )
            .await
            {
                Ok(Some(m)) => validated.push(LocationCandidate {
                    mbid: mbid.clone(),
                    mb_name: mb_name.clone(),
                    qobuz_id: Some(m.qobuz_id as i64),
                    qobuz_name: Some(m.name),
                    qobuz_image: m.image,
                    score: *score,
                    genres: candidate_genres.clone(),
                    qobuz_albums_count: m.albums_count,
                }),
                Ok(None) => {}
                Err(e) => {
                    // Terminal classes end the page here rather than burning
                    // up to 99 more searches that will fail the same way.
                    if let Some(fatal) = catalog.take_fatal() {
                        log::warn!("[scene] Qobuz validation aborted: {e}");
                        return Err(fatal);
                    }
                    log::warn!("[scene] Qobuz validation failed for {mb_name:?}: {e}");
                    validation_failures += 1;
                    last_validation_error = e.to_string();
                }
            }
        }

        // There were candidates and not one could be checked — the catalog is
        // unreachable. Distinct from "checked them all, none are on Qobuz",
        // which is a legitimately empty scene.
        if total_to_validate > 0 && validation_failures == total_to_validate {
            return Err(SceneDiscoveryError::CatalogUnavailable(
                last_validation_error,
            ));
        }

        progress(
            ScenePhase::Done,
            100,
            format!("{} artists", validated.len()),
        );

        let scene_label = country
            .map(|c| c.to_string())
            .unwrap_or_else(|| display_name.clone());
        let next_offset = offset + to_validate.len();
        let response = LocationDiscoveryResponse {
            artists: validated,
            scene_label,
            genre_summary: genre_summary(&source_seeds),
            total_candidates,
            has_more: next_offset < total_candidates,
            next_offset,
        };

        // First page only, non-empty only — Tauri's rule. The stored rows are
        // already blacklist-aware; the post-filter on read catches anything
        // blocked afterwards, and `invalidate_scene_cache` covers un-blocking.
        if let (0, false, Some(cache_key)) =
            (offset, response.artists.is_empty(), cache_key.as_deref())
        {
            if let Ok(guard) = self.musicbrainz_cache.lock() {
                if let Some(cache) = guard.as_ref() {
                    let _ = cache.set_scene_cache(cache_key, &response);
                }
            }
        }

        Ok(response)
    }

    /// Re-derive every cached scene row's Qobuz projection from the
    /// identifier-keyed identity cache (cheap on a live row; a cheap Qobuz
    /// re-search on an expired provisional one). Rows that no longer resolve
    /// are dropped. Returns the number dropped.
    async fn revalidate_scene_rows(
        &self,
        hit: &mut LocationDiscoveryResponse,
        store: &CoreIdentityStore<'_>,
        catalog: &SceneQobuz<'_, A>,
        scope: Option<&str>,
        blacklist: &(dyn Fn(u64) -> bool + Send + Sync),
    ) -> Result<usize, SceneDiscoveryError> {
        let before = hit.artists.len();
        let mut kept: Vec<LocationCandidate> = Vec::with_capacity(before);
        for mut row in std::mem::take(&mut hit.artists) {
            match identity::resolve_scene_candidate(
                Some(store),
                self.musicbrainz.as_ref(),
                catalog,
                &row.mbid,
                &row.mb_name,
                scope,
                blacklist,
            )
            .await
            {
                Ok(Some(m)) => {
                    row.qobuz_id = Some(m.qobuz_id as i64);
                    row.qobuz_name = Some(m.name);
                    row.qobuz_image = m.image;
                    row.qobuz_albums_count = m.albums_count;
                    kept.push(row);
                }
                Ok(None) => {}
                Err(e) => {
                    if let Some(fatal) = catalog.take_fatal() {
                        return Err(fatal);
                    }
                    // Transient: keep the cached projection for this row.
                    log::warn!("[scene] revalidation of {:?} failed, keeping cached row: {e}", row.mb_name);
                    kept.push(row);
                }
            }
        }
        let dropped = before.saturating_sub(kept.len());
        hit.artists = kept;
        Ok(dropped)
    }
}

// ----- #768 identity boundaries ----------------------------------------------

/// `identity::IdentityStore` over the core's optional SQLite cache. Every
/// failure is logged and reads as a miss; without a cache the matcher simply
/// runs uncached.
struct CoreIdentityStore<'a>(&'a std::sync::Mutex<Option<MusicBrainzCache>>);

impl CoreIdentityStore<'_> {
    fn with<T>(
        &self,
        what: &str,
        f: impl FnOnce(&MusicBrainzCache) -> Result<T, String>,
    ) -> Option<T> {
        let guard = self.0.lock().ok()?;
        let cache = guard.as_ref()?;
        match f(cache) {
            Ok(v) => Some(v),
            Err(e) => {
                log::warn!("[mb-identity] cache {what}: {e}");
                None
            }
        }
    }
}

impl identity::IdentityStore for CoreIdentityStore<'_> {
    fn artist_query(&self, key: &str) -> Option<Vec<ArtistCandidate>> {
        self.with("artist_query", |c| c.get_artist_query(key)).flatten()
    }
    fn put_artist_query(&self, key: &str, window: &[ArtistCandidate]) {
        self.with("put_artist_query", |c| c.set_artist_query(key, window));
    }
    fn isrc_credits(&self, isrc: &str) -> Option<IsrcCredits> {
        self.with("isrc_credits", |c| c.get_isrc_credits(isrc)).flatten()
    }
    fn put_isrc_credits(&self, credits: &IsrcCredits) {
        self.with("put_isrc_credits", |c| c.set_isrc_credits(credits));
    }
    fn source_identity(&self, qobuz_artist_id: u64) -> Option<SourceIdentityRow> {
        self.with("source_identity", |c| c.get_source_identity(qobuz_artist_id))
            .flatten()
    }
    fn put_source_identity(&self, row: &SourceIdentityRow) {
        self.with("put_source_identity", |c| c.set_source_identity(row));
    }
    fn scene_identity(&self, mbid: &str, scope: &str) -> Option<SceneIdentityRow> {
        self.with("scene_identity", |c| c.get_scene_identity(mbid, scope))
            .flatten()
    }
    fn put_scene_identity(&self, row: &SceneIdentityRow) {
        self.with("put_scene_identity", |c| c.set_scene_identity(row));
    }
}

/// `identity::SceneCatalog` over the core's Qobuz client. A Qobuz failure
/// that will refuse every further call (auth/region, `scene_fatal_qobuz`)
/// is parked in `fatal` so the validation loop can end the page instead of
/// burning the remaining searches.
struct SceneQobuz<'a, A: FrontendAdapter + Send + Sync + 'static> {
    core: &'a QbzCore<A>,
    fatal: std::sync::Mutex<Option<SceneDiscoveryError>>,
}

impl<A: FrontendAdapter + Send + Sync + 'static> SceneQobuz<'_, A> {
    fn park(&self, e: &CoreError) -> FetchError {
        if let Some(fatal) = scene_fatal_qobuz(e) {
            if let Ok(mut slot) = self.fatal.lock() {
                *slot = Some(fatal);
            }
        }
        FetchError::Unavailable(e.to_string())
    }

    fn take_fatal(&self) -> Option<SceneDiscoveryError> {
        self.fatal.lock().ok().and_then(|mut slot| slot.take())
    }
}

impl<A: FrontendAdapter + Send + Sync + 'static> identity::SceneCatalog for SceneQobuz<'_, A> {
    /// The bounded Qobuz search (five results, unchanged) filtered to exact
    /// normalised names. The blacklist is applied by the matcher.
    fn exact_name_candidates<'a>(
        &'a self,
        name: &'a str,
    ) -> BoxFuture<'a, Result<Vec<SceneQobuzCandidate>, FetchError>> {
        Box::pin(async move {
            let results = self
                .core
                .search_artists(name, 5, 0, None)
                .await
                .map_err(|e| self.park(&e))?;
            let norm = normalize_artist_name(name);
            Ok(results
                .items
                .iter()
                .filter(|a| normalize_artist_name(&a.name) == norm)
                .map(|a| SceneQobuzCandidate {
                    qobuz_id: a.id,
                    name: a.name.clone(),
                    image: a
                        .image
                        .as_ref()
                        .and_then(|img| img.small.as_ref().or(img.thumbnail.as_ref()).cloned()),
                    albums_count: a.albums_count,
                })
                .collect())
        })
    }

    /// One `/artist/page` per AMBIGUOUS candidate, its own top-track ISRCs
    /// only (guest appearances excluded), capped by the matcher.
    fn candidate_isrcs<'a>(
        &'a self,
        qobuz_id: u64,
        cap: usize,
    ) -> BoxFuture<'a, Result<Vec<String>, FetchError>> {
        Box::pin(async move {
            let page = self
                .core
                .get_artist_page(qobuz_id, None)
                .await
                .map_err(|e| self.park(&e))?;
            let tracks = page.top_tracks.unwrap_or_default();
            Ok(identity::pick_source_isrcs(
                qobuz_id,
                tracks
                    .iter()
                    .map(|t| (t.isrc.as_deref(), t.artist.as_ref().map(|a| a.id))),
                cap,
            ))
        })
    }
}

// ----- Musician resolution + appearances -----------------------------------

impl<A: FrontendAdapter + Send + Sync + 'static> QbzCore<A> {
    /// Resolve a musician (band member / collaborator) to the
    /// strongest available identity: MBID via MusicBrainz, exact-
    /// name Qobuz artist match, or just an appears-on count. Ports
    /// v2_resolve_musician end to end so the artist-network sidebar
    /// click can route the user to the right view (artist page when
    /// confident, MusicianPageView otherwise).
    pub async fn musicbrainz_resolve_musician(
        &self,
        name: &str,
        role: &str,
    ) -> Result<ResolvedMusician, CoreError> {
        // MusicBrainz opt-out / unavailable: a disabled or failed resolve is NOT
        // fatal here — fall through to the Qobuz exact-name / appears-on fallback
        // below so the musician page still degrades cleanly when MB is off. (Was
        // `?`, which propagated the disabled-client Err before the Qobuz fallback
        // and left the musician page blank/stuck when MB is disabled.)
        let resolved_artist = match self.musicbrainz.resolve_artist(name).await {
            Ok(found) => found,
            Err(e) => {
                log::debug!(
                    "musicbrainz_resolve_musician: MB resolve unavailable for {name:?}: {e}"
                );
                None
            }
        };

        let normalized_target = name.trim().to_lowercase();
        let artist_results = self.search_artists(name, 10, 0, None).await?;
        let exact = artist_results
            .items
            .iter()
            .find(|artist| artist.name.trim().to_lowercase() == normalized_target);

        if let Some(artist) = exact {
            let qobuz_artist_id = i64::try_from(artist.id).ok();
            return Ok(ResolvedMusician {
                name: name.to_string(),
                role: role.to_string(),
                mbid: None,
                qobuz_artist_id,
                confidence: MusicianConfidence::Confirmed,
                bands: Vec::new(),
                appears_on_count: 0,
            });
        }

        let album_results = self.search_albums(name, 20, 0, None).await?;
        let appears_on_count = album_results.total as usize;

        let confidence = if appears_on_count > 0 {
            MusicianConfidence::Contextual
        } else if resolved_artist.is_some() {
            MusicianConfidence::Weak
        } else {
            MusicianConfidence::None
        };

        Ok(ResolvedMusician {
            name: name.to_string(),
            role: role.to_string(),
            mbid: resolved_artist.as_ref().map(|a| a.mbid.clone()),
            qobuz_artist_id: None,
            confidence,
            bands: Vec::new(),
            appears_on_count,
        })
    }

    /// Fetch the paginated list of albums on which `name` appears,
    /// for the Appears On grid in MusicianPageView. Ports
    /// v2_get_musician_appearances — same Qobuz album search +
    /// per-row mapping with `role_on_album = role`.
    pub async fn musicbrainz_get_musician_appearances(
        &self,
        name: &str,
        role: &str,
        limit: u32,
        offset: u32,
    ) -> Result<MusicianAppearances, CoreError> {
        let results = self.search_albums(name, limit, offset, None).await?;
        let albums = results
            .items
            .into_iter()
            .map(|album| AlbumAppearance {
                album_id: album.id,
                album_title: album.title,
                album_artwork: album.image.large.or(album.image.small).unwrap_or_default(),
                artist_name: album.artist.name,
                year: album.release_date_original,
                role_on_album: role.to_string(),
            })
            .collect::<Vec<_>>();
        Ok(MusicianAppearances {
            albums,
            total: results.total as usize,
        })
    }
}

/// Normalize an artist name for dedupe: trim, lowercase, collapse
/// whitespace. Used by the discovery pipeline so "Iron  Maiden" and
/// "iron maiden" hash to the same key in the dismiss store.
pub fn normalize_artist_name(name: &str) -> String {
    name.trim()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Deterministic shuffle keyed by `seed_mbid` (and optionally a tag).
/// Same artist page produces the same order across runs; different
/// artist or different fallback tag produces a different order.
fn shuffle_with_seed<T>(items: &mut Vec<T>, seed_mbid: &str, secondary_tag: Option<&str>) {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    seed_mbid.hash(&mut hasher);
    if let Some(t) = secondary_tag {
        t.hash(&mut hasher);
    }
    let mut h = hasher.finish();
    // Fisher-Yates with a simple xorshift PRNG seeded from the hash —
    // keeps qbz-core free of the rand dep.
    let n = items.len();
    for i in (1..n).rev() {
        h ^= h << 13;
        h ^= h >> 7;
        h ^= h << 17;
        let j = (h % ((i + 1) as u64)) as usize;
        items.swap(i, j);
    }
}

/// Pure set-intersection behind [`QbzCore::check_playlist_duplicates`] — split
/// out so the duplicate logic is unit-testable without a live Qobuz client.
/// `existing` = the playlist's current track ids; `track_ids` = the ids the
/// user wants to add. Returns the Tauri-shaped result (total checked, how many
/// are already present, and the set of those duplicate ids).
pub(crate) fn compute_playlist_duplicates(
    existing: &[u64],
    track_ids: &[u64],
) -> PlaylistDuplicateResult {
    let existing_set: std::collections::HashSet<u64> = existing.iter().copied().collect();
    let duplicate_track_ids: std::collections::HashSet<u64> = track_ids
        .iter()
        .copied()
        .filter(|id| existing_set.contains(id))
        .collect();
    PlaylistDuplicateResult {
        total_tracks: track_ids.len(),
        duplicate_count: duplicate_track_ids.len(),
        duplicate_track_ids,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_search_all_filters_blacklisted_artist() {
        let json = serde_json::json!({
            "albums":  { "items": [], "total": 0, "offset": 0, "limit": 30 },
            "tracks":  { "items": [], "total": 0, "offset": 0, "limit": 30 },
            "artists": {
                "items": [
                    { "id": 1, "name": "Keep" },
                    { "id": 999, "name": "Blocked" }
                ],
                "total": 2, "offset": 0, "limit": 30
            },
            "playlists": { "items": [], "total": 0, "offset": 0, "limit": 30 }
        });
        let blocked: BlacklistFilter = [999].into_iter().collect();
        let out = parse_search_all(&json, &blocked, &no_albums());
        assert_eq!(out.artists.items.len(), 1);
        assert_eq!(out.artists.items[0].name, "Keep");
        assert_eq!(out.artists.total, 1);
        assert!(out.most_popular.is_none());
    }

    #[test]
    fn parse_page_skips_poisoned_item_keeps_rest() {
        // One malformed entry (id is a string where Artist.id: u64) must NOT
        // blank the page — the old whole-page from_value did exactly that.
        let json = serde_json::json!({
            "artists": {
                "items": [
                    { "id": 1, "name": "Keep" },
                    { "id": "poisoned", "name": "Bad" },
                    { "id": 2, "name": "AlsoKeep" }
                ],
                "total": 3, "offset": 0, "limit": 30
            }
        });
        let page = parse_page::<Artist>(&json, "artists");
        assert_eq!(page.items.len(), 2);
        assert_eq!(page.items[0].name, "Keep");
        assert_eq!(page.items[1].name, "AlsoKeep");
        assert_eq!(page.total, 3);
        assert_eq!(page.limit, 30);
    }

    // --- D-FEAT featured-aware blacklist helpers ---

    use qbz_models::types::{AlbumArtist, AlbumSummary};
    use qbz_models::{DiscoverAlbumImage, DiscoverArtist};

    /// Empty album-blacklist filter for the artist-axis tests (the album axis
    /// is exercised by the dedicated album-id tests below).
    fn no_albums() -> AlbumBlacklistFilter {
        AlbumBlacklistFilter::new()
    }

    // Album and Track do not derive Default in qbz-models, so test fixtures
    // construct full struct literals. Only the artist-id fields are meaningful
    // to the blacklist helpers; everything else is zero/None filler.
    fn album_with_artists(primary_id: u64, featured_ids: &[u64]) -> Album {
        let artists = std::iter::once(AlbumArtist {
            id: primary_id,
            name: String::new(),
            roles: Some(vec!["main-artist".to_string()]),
        })
        .chain(featured_ids.iter().map(|&id| AlbumArtist {
            id,
            name: String::new(),
            roles: Some(vec!["featured-artist".to_string()]),
        }))
        .collect();
        Album {
            favorited_at: None,
            purchasable: None,
            id: String::new(),
            title: String::new(),
            artist: Artist {
                id: primary_id,
                ..Default::default()
            },
            image: Default::default(),
            release_date_original: None,
            release_date_stream: None,
            streamable: None,
            label: None,
            genre: None,
            tracks_count: None,
            duration: None,
            hires: false,
            hires_streamable: false,
            maximum_sampling_rate: None,
            maximum_bit_depth: None,
            audio_info: None,
            dates: None,
            track_count: None,
            release_type: None,
            tracks: None,
            upc: None,
            description: None,
            goodies: None,
            awards: None,
            parental_warning: None,
            artists: Some(artists),
            composer: None,
            version: None,
        }
    }

    fn track_with(performer_id: Option<u64>, composer_id: Option<u64>) -> Track {
        Track {
            favorited_at: None,
            purchasable: None,
            id: 0,
            title: String::new(),
            version: None,
            isrc: None,
            audio_info: None,
            duration: 0,
            track_number: 0,
            media_number: None,
            performer: performer_id.map(|id| Artist {
                id,
                ..Default::default()
            }),
            album: None,
            hires: false,
            hires_streamable: false,
            maximum_sampling_rate: None,
            maximum_bit_depth: None,
            // `None` = the fixture makes no availability claim, which is the
            // honest value: nothing in `track_with`'s callers reads it.
            streamable: None,
            streamable_at: None,
            release_date_stream: None,
            parental_warning: false,
            playlist_track_id: None,
            performers: None,
            composer: composer_id.map(|id| Artist {
                id,
                ..Default::default()
            }),
            copyright: None,
            work: None,
        }
    }

    #[test]
    fn album_blacklisted_blocks_on_primary_artist() {
        let album = album_with_artists(1, &[]);
        let bl: BlacklistFilter = [1].into_iter().collect();
        assert!(album_blacklisted(&album, &bl, &no_albums()));
    }

    #[test]
    fn album_blacklisted_blocks_on_featured_not_primary() {
        // Primary is 1 (kept), featured 999 is blocked.
        let album = album_with_artists(1, &[999]);
        let bl: BlacklistFilter = [999].into_iter().collect();
        assert!(album_blacklisted(&album, &bl, &no_albums()));
    }

    #[test]
    fn album_blacklisted_keeps_when_no_match() {
        let album = album_with_artists(1, &[2, 3]);
        let bl: BlacklistFilter = [999].into_iter().collect();
        assert!(!album_blacklisted(&album, &bl, &no_albums()));
    }

    #[test]
    fn album_blacklisted_empty_filter_is_false() {
        let album = album_with_artists(1, &[999]);
        let bl: BlacklistFilter = BlacklistFilter::new();
        assert!(!album_blacklisted(&album, &bl, &no_albums()));
    }

    #[test]
    fn track_blacklisted_blocks_on_performer() {
        let track = track_with(Some(5), None);
        let bl: BlacklistFilter = [5].into_iter().collect();
        assert!(track_blacklisted(&track, &bl, &no_albums()));
    }

    #[test]
    fn track_blacklisted_blocks_on_composer() {
        let track = track_with(Some(1), Some(7));
        let bl: BlacklistFilter = [7].into_iter().collect();
        assert!(track_blacklisted(&track, &bl, &no_albums()));
    }

    #[test]
    fn track_blacklisted_keeps_when_no_match() {
        let track = track_with(Some(1), Some(2));
        let bl: BlacklistFilter = [999].into_iter().collect();
        assert!(!track_blacklisted(&track, &bl, &no_albums()));
    }

    #[test]
    fn track_blacklisted_fail_open_when_no_ids() {
        // No performer + no composer => kept (fail-open).
        let track = track_with(None, None);
        let bl: BlacklistFilter = [1, 2, 3].into_iter().collect();
        assert!(!track_blacklisted(&track, &bl, &no_albums()));
    }

    #[test]
    fn track_blacklisted_empty_filter_is_false() {
        let track = track_with(Some(5), Some(7));
        let bl: BlacklistFilter = BlacklistFilter::new();
        assert!(!track_blacklisted(&track, &bl, &no_albums()));
    }

    #[test]
    fn discover_album_blacklisted_blocks_on_any_artist() {
        let album = DiscoverAlbum {
            id: String::new(),
            title: String::new(),
            version: None,
            track_count: None,
            duration: None,
            parental_warning: None,
            image: DiscoverAlbumImage {
                small: None,
                thumbnail: None,
                large: None,
            },
            artists: vec![
                DiscoverArtist {
                    id: 1,
                    name: String::new(),
                    roles: None,
                },
                DiscoverArtist {
                    id: 999,
                    name: String::new(),
                    roles: None,
                },
            ],
            label: None,
            genre: None,
            dates: None,
            audio_info: None,
            awards: None,
        };
        let blocked: BlacklistFilter = [999].into_iter().collect();
        assert!(discover_album_blacklisted(&album, &blocked, &no_albums()));
        let kept: BlacklistFilter = [555].into_iter().collect();
        assert!(!discover_album_blacklisted(&album, &kept, &no_albums()));
    }

    // --- Album-id axis (orthogonal to the artist axis) ---

    fn album_with_id(id: &str, primary_artist: u64) -> Album {
        let mut a = album_with_artists(primary_artist, &[]);
        a.id = id.to_string();
        a
    }

    fn track_with_album(album_id: &str) -> Track {
        let mut t = track_with(Some(1), Some(2));
        t.album = Some(AlbumSummary {
            id: album_id.to_string(),
            title: String::new(),
            image: Default::default(),
            label: None,
            genre: None,
            artist: None,
            version: None,
            streamable: None,
            streamable_at: None,
            release_date_stream: None,
            release_date_original: None,
        });
        t
    }

    #[test]
    fn album_blocked_by_own_id_regardless_of_artist() {
        // Artist filter EMPTY; only the album id is blocked. The widened
        // fail-open guard must NOT early-return here.
        let album = album_with_id("blk", 1);
        let abl: AlbumBlacklistFilter = ["blk".to_string()].into_iter().collect();
        assert!(album_blacklisted(&album, &BlacklistFilter::new(), &abl));
        let other: AlbumBlacklistFilter = ["zzz".to_string()].into_iter().collect();
        assert!(!album_blacklisted(&album, &BlacklistFilter::new(), &other));
    }

    #[test]
    fn album_blocked_keeps_sibling_album_of_same_artist() {
        // The merged-artist use case: blocking one album by id leaves the same
        // artist's other releases visible.
        let blocked_album = album_with_id("bad", 1);
        let good_album = album_with_id("good", 1);
        let abl: AlbumBlacklistFilter = ["bad".to_string()].into_iter().collect();
        assert!(album_blacklisted(&blocked_album, &BlacklistFilter::new(), &abl));
        assert!(!album_blacklisted(&good_album, &BlacklistFilter::new(), &abl));
    }

    #[test]
    fn track_blocked_by_album_id() {
        let track = track_with_album("blk");
        let abl: AlbumBlacklistFilter = ["blk".to_string()].into_iter().collect();
        assert!(track_blacklisted(&track, &BlacklistFilter::new(), &abl));
        // No album / different id => kept.
        assert!(!track_blacklisted(
            &track_with(Some(1), None),
            &BlacklistFilter::new(),
            &abl
        ));
    }

    #[test]
    fn discover_album_blocked_by_own_id() {
        let mut album = DiscoverAlbum {
            id: "blk".to_string(),
            title: String::new(),
            version: None,
            track_count: None,
            duration: None,
            parental_warning: None,
            image: DiscoverAlbumImage {
                small: None,
                thumbnail: None,
                large: None,
            },
            artists: vec![DiscoverArtist {
                id: 1,
                name: String::new(),
                roles: None,
            }],
            label: None,
            genre: None,
            dates: None,
            audio_info: None,
            awards: None,
        };
        let abl: AlbumBlacklistFilter = ["blk".to_string()].into_iter().collect();
        assert!(discover_album_blacklisted(&album, &BlacklistFilter::new(), &abl));
        album.id = "other".to_string();
        assert!(!discover_album_blacklisted(&album, &BlacklistFilter::new(), &abl));
    }

    #[test]
    fn parse_search_all_filters_blocked_album() {
        let json = serde_json::json!({
            "albums": {
                "items": [
                    { "id": "keep", "title": "Keep", "artist": { "id": 1, "name": "A" } },
                    { "id": "blk",  "title": "Bogus", "artist": { "id": 1, "name": "A" } }
                ],
                "total": 2, "offset": 0, "limit": 30
            },
            "tracks":  { "items": [], "total": 0, "offset": 0, "limit": 30 },
            "artists": { "items": [], "total": 0, "offset": 0, "limit": 30 },
            "playlists": { "items": [], "total": 0, "offset": 0, "limit": 30 }
        });
        let abl: AlbumBlacklistFilter = ["blk".to_string()].into_iter().collect();
        let out = parse_search_all(&json, &BlacklistFilter::new(), &abl);
        assert_eq!(out.albums.items.len(), 1);
        assert_eq!(out.albums.items[0].id, "keep");
        assert_eq!(out.albums.total, 1);
    }

    #[test]
    fn compute_playlist_duplicates_intersects_input_with_existing() {
        // Existing playlist has 10, 20, 30. Adding 20, 30, 40, 50:
        // 20 and 30 are duplicates; 40 and 50 are new.
        let existing = [10u64, 20, 30];
        let to_add = [20u64, 30, 40, 50];
        let r = compute_playlist_duplicates(&existing, &to_add);
        assert_eq!(r.total_tracks, 4);
        assert_eq!(r.duplicate_count, 2);
        assert!(r.duplicate_track_ids.contains(&20));
        assert!(r.duplicate_track_ids.contains(&30));
        assert!(!r.duplicate_track_ids.contains(&40));
    }

    #[test]
    fn compute_playlist_duplicates_none_when_disjoint() {
        let r = compute_playlist_duplicates(&[1u64, 2, 3], &[4u64, 5]);
        assert_eq!(r.total_tracks, 2);
        assert_eq!(r.duplicate_count, 0);
        assert!(r.duplicate_track_ids.is_empty());
    }

    #[test]
    fn compute_playlist_duplicates_empty_input() {
        let r = compute_playlist_duplicates(&[1u64, 2, 3], &[]);
        assert_eq!(r.total_tracks, 0);
        assert_eq!(r.duplicate_count, 0);
    }
}
