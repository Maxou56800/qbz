//! QBZ Qobuz - Qobuz API client
//!
//! This crate provides the Qobuz API client:
//! - QobuzClient: Main API interface
//! - Authentication and token management
//! - Search, catalog browsing, streaming URLs
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────────────────────────────────────────────────┐
//! │                     qbz-qobuz (Tier 1)                      │
//! │  Qobuz API client, authentication, streaming URLs          │
//! └─────────────────────────────────────────────────────────────┘
//!                              ↑
//!                      ┌───────┴───────┐
//!                      │  qbz-models   │
//!                      │   (Tier 0)    │
//!                      └───────────────┘
//! ```
//!
//! # Usage
//!
//! ```rust,ignore
//! use qbz_qobuz::QobuzClient;
//!
//! let client = QobuzClient::new()?;
//! client.init().await?;
//! let session = client.login("email", "password").await?;
//! let stream_url = client.get_stream_url(track_id, quality).await?;
//! ```

pub mod auth;
pub mod bundle;
pub mod client;
pub mod cmaf;
pub mod delegated;
pub mod endpoints;
pub mod error;
pub mod forbidden_breaker;
pub mod link_resolver;
pub mod lyrics;
pub mod net_diag;
pub mod offline_gate;
pub mod performers;
pub mod purchases;
pub mod qconnect_auth;
pub mod retry;

// Re-export main types
pub use bundle::BundleTokens;
pub use client::QobuzClient;
pub use cmaf::{
    decrypt_segments_into, download_full as cmaf_download_full,
    download_full_with_progress as cmaf_download_full_with_progress,
    download_raw as cmaf_download_raw,
    download_raw_with_progress as cmaf_download_raw_with_progress,
    setup_streaming as cmaf_setup_streaming, CmafProgressCallback, CmafProgressUpdate,
    CmafRawBundle, CmafStreamingInfo, CMAF_PREFETCH_CONCURRENCY,
};
pub use delegated::{
    DelegatedApiConfig, DelegatedApiConfigError, DelegatedApiEndpoint, DelegatedApiError,
    DelegatedApiResult, DelegatedAppCredentials, DelegatedQobuzClient,
};
pub use error::{ApiError, Result};
pub use link_resolver::{resolve_link, LinkResolverError, ResolvedLink};
pub use lyrics::{
    merge_translation_into, QobuzLyricsContent, QobuzLyricsDocument, QobuzLyricsLine,
    QobuzLyricsPlainLine, QobuzLyricsPublisher, QobuzLyricsTranslation, QobuzLyricsUrls,
    QobuzLyricsWord,
};
pub use qconnect_auth::QwsAuthToken;
