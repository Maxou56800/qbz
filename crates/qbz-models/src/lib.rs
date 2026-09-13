//! QBZ Models - Shared types, events, and traits
//!
//! This crate provides the foundation for all QBZ crates:
//! - Type definitions (Track, Album, Artist, etc.)
//! - Event definitions (CoreEvent enum)
//! - Trait definitions (FrontendAdapter)
//! - Playback types (QueueTrack, PlaybackState)
//! - Error types
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                      qbz-models (Tier 0)                    │
//! │  Types, Events, Traits - No dependencies on other qbz-*    │
//! └─────────────────────────────────────────────────────────────┘
//!                              ↑
//!     ┌────────────────────────┼────────────────────────┐
//!     │                        │                        │
//! ┌───┴───┐              ┌─────┴─────┐            ┌─────┴─────┐
//! │qbz-audio│            │qbz-qobuz  │            │qbz-player │
//! │ Tier 1 │             │  Tier 1   │            │  Tier 2   │
//! └────────┘             └───────────┘            └───────────┘
//! ```
//!
//! # Usage
//!
//! ```rust
//! use qbz_models::{Track, Album, CoreEvent, FrontendAdapter};
//! ```

pub mod error;
pub mod events;
pub mod fs_url;
pub mod lenient;
pub mod mixtape;
pub mod playback;
pub mod playback_cache;
pub mod purchase_serde;
pub mod source;
pub mod traits;
pub mod types;

// Re-export commonly used types at crate root
pub use error::{QbzError, QbzResult};
pub use events::CoreEvent;
pub use lenient::{parse_items_array, parse_items_lenient};
pub use playback::{PlaybackState, PlaybackStatus, QueueState, QueueTrack, RepeatMode};
pub use source::{plex_thumb_url, ArtworkRef, PlaybackSource, TrackOriginTag};
pub use traits::{FrontendAdapter, LoggingAdapter, NoOpAdapter};
pub use types::{
    Album,
    AlbumSuggestResponse,
    AlbumSummary,
    Artist,
    RadioResponse,
    ArtistAlbums,
    ArtistBiography,
    Goody,
    // Discover types
    DiscoverAlbum,
    DiscoverAlbumDates,
    DiscoverAlbumImage,
    DiscoverArtist,
    DiscoverAudioInfo,
    DiscoverContainer,
    DiscoverContainers,
    DiscoverData,
    DiscoverPlaylist,
    DiscoverPlaylistImage,
    DiscoverPlaylistsResponse,
    DiscoverResponse,
    Favorites,
    Genre,
    GenreInfo,
    GenreListContainer,
    GenreListResponse,
    ImageSet,
    Label,
    LabelExploreResponse,
    LabelGetListResponse,
    LabelListPage,
    LabelPageContainer,
    LabelPageData,
    LabelPageGenericList,
    LabelStoryResponse,
    // Award types
    AlbumAward,
    AwardMagazine,
    AwardPageContainer,
    AwardPageData,
    AwardPageGenericList,
    // Artist page types
    ArtistStoryResponse,
    ArtistStoryItem,
    ArtistStoryImage,
    ArtistStoryAuthor,
    PageArtistAward,
    PageArtistBiography,
    PageArtistImages,
    PageArtistName,
    PageArtistPhysicalSupport,
    PageArtistPlaylist,
    PageArtistPlaylistImages,
    PageArtistPlaylistOwner,
    PageArtistPlaylists,
    PageArtistPortrait,
    PageArtistRelease,
    PageArtistReleaseArtist,
    PageArtistReleaseContributor,
    PageArtistReleaseGroup,
    PageArtistResponse,
    PageArtistRights,
    PageArtistSimilar,
    PageArtistSimilarItem,
    PageArtistTrack,
    PageArtistTrackAlbum,
    Playlist,
    // Purchase types
    PurchaseAlbum,
    PurchaseFormatOption,
    PurchaseIdsResponse,
    PurchaseResponse,
    PurchaseTrack,
    PlaylistDuplicateResult,
    PlaylistGenre,
    PlaylistOwner,
    PlaylistTag,
    PlaylistTagsResponse,
    PlaylistWithTrackIds,
    Quality,
    QualityLimit,
    RawPlaylistTag,
    ReleasesGridResponse,
    MostPopularItem,
    SearchAllResults,
    SearchResults,
    SearchResultsPage,
    SessionStartResponse,
    AssetOrigin,
    AudioParams,
    probe_streaminfo,
    qobuz_cover_at_px,
    ExternalStreamAsset,
    StreamQualityInfo,
    StreamRestriction,
    StreamUrl,
    Track,
    TrackFileUrl,
    TrackToAnalyse,
    TracksContainer,
    Entitlements,
    UserSession,
};
