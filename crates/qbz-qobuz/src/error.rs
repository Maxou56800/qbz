//! API error types

use thiserror::Error;

#[derive(Error, Debug)]
pub enum ApiError {
    #[error("Authentication failed: {0}")]
    AuthenticationError(String),

    #[error("Invalid app ID")]
    InvalidAppId,

    #[error("Invalid app secret")]
    InvalidAppSecret,

    #[error("Failed to extract bundle tokens: {0}")]
    BundleExtractionError(String),

    #[error("User is not eligible (no active subscription)")]
    IneligibleUser,

    #[error("Track is not streamable")]
    NonStreamable,

    #[error("Invalid quality format: {0}")]
    InvalidQuality(u32),

    #[error("No valid quality available for this track")]
    NoQualityAvailable,

    #[error("Track {0} is no longer available on Qobuz")]
    TrackUnavailable(u64),

    /// Qobuz answered 403 Forbidden on an authenticated request. The account is
    /// authenticated but not currently allowed to perform the action (entitlement
    /// not restored after an outage, geo/concurrency limit, or an edge/WAF block).
    /// The string contains only a redacted body-presence diagnostic. Terminal,
    /// never a per-quality restriction — abort the fallback loop instead of
    /// retrying.
    #[error("Access forbidden by Qobuz (HTTP 403){0}")]
    Forbidden(String),

    /// The 403 circuit breaker is open after repeated forbidden responses; the
    /// request was short-circuited WITHOUT touching the network so we don't get
    /// the IP edge-blocked. Clears itself after a cooldown. See [`crate::forbidden_breaker`].
    #[error("Temporarily backing off after repeated 403s ({0}s remaining)")]
    ForbiddenCircuitOpen(u64),

    #[error("Offline mode is active - Qobuz services are disabled")]
    OfflineMode,

    #[error("Network error: {0}")]
    NetworkError(#[from] reqwest::Error),

    #[error("Invalid proxy configuration: {0}")]
    InvalidProxyConfig(#[from] qbz_net_proxy::ProxyConfigError),

    #[error("JSON parsing error: {0}")]
    ParseError(#[from] serde_json::Error),

    #[error("API error: {0}")]
    ApiResponse(String),

    #[error("Rate limited, retry after {0} seconds")]
    RateLimited(u64),

    #[error("Server error (HTTP {0})")]
    ServerError(u16),
}

impl ApiError {
    /// Whether this is the exact terminal 404 sentinel emitted by
    /// `QobuzClient::get_album`. Centralising the legacy string-shaped result
    /// keeps UI callers from broad `contains("404")` guesses while avoiding a
    /// new enum variant that would break every exhaustive error classifier.
    pub fn is_album_unavailable(&self, album_id: &str) -> bool {
        matches!(
            self,
            ApiError::ApiResponse(message)
                if message == &format!("Album {album_id} not found (404)")
        )
    }

    /// True for errors worth retrying with backoff (issue #467): transport
    /// problems (timeout/connect/reset), 5xx server errors, and 429 rate
    /// limiting. Terminal errors — a real 404 `TrackUnavailable`, auth or
    /// parse failures — return false and should propagate to the (bounded)
    /// skip path instead of being retried.
    pub fn is_transient(&self) -> bool {
        match self {
            ApiError::NetworkError(e) => crate::retry::reqwest_is_transient(e),
            ApiError::RateLimited(_) | ApiError::ServerError(_) => true,
            _ => false,
        }
    }
}

pub type Result<T> = std::result::Result<T, ApiError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn album_unavailable_recognises_only_the_exact_get_album_404() {
        assert!(ApiError::ApiResponse("Album old-id not found (404)".into())
            .is_album_unavailable("old-id"));
        assert!(
            !ApiError::ApiResponse("get_album(old-id) status 500".into())
                .is_album_unavailable("old-id")
        );
        assert!(
            !ApiError::ApiResponse("Album another-id not found (404)".into())
                .is_album_unavailable("old-id")
        );
        assert!(!ApiError::OfflineMode.is_album_unavailable("old-id"));
    }
}
