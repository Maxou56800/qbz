//! Isolated Qobuz API access for a delegated QConnect guest session.
//!
//! This client owns its HTTP stack and credentials. It never reads or changes
//! [`crate::QobuzClient`] owner-session state, OAuth credentials, keyrings, or
//! global headers. The delegated JWT is sent only as `Authorization: Bearer`,
//! alongside `X-App-Id`; `X-User-Auth-Token` is deliberately unavailable.
//!
//! The surface is intentionally read-only and transient: access validation,
//! track metadata, and stream URL resolution. There is no persistence, token
//! refresh, library/account operation, or implicit owner fallback here.

use crate::auth::get_timestamp;
use crate::endpoints::paths;
use md5::{Digest, Md5};
use qbz_models::{Quality, StreamRestriction, StreamUrl, Track};
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use reqwest::{Client, RequestBuilder, Response, StatusCode, Url};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use thiserror::Error;
use zeroize::Zeroizing;

const MAX_ENDPOINT_BYTES: usize = 2_048;
const MAX_JWT_BYTES: usize = 16 * 1_024;
const MAX_APP_ID_BYTES: usize = 256;
const MAX_SIGNING_SECRET_BYTES: usize = 16 * 1_024;
const MIN_INITIAL_TTL: Duration = Duration::from_secs(60);
const TRACKS_PER_BATCH: usize = 50;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// A configuration rejection which never embeds the rejected value.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum DelegatedApiConfigError {
    #[error("delegated API endpoint is invalid")]
    InvalidEndpoint,
    #[error("delegated API endpoint exceeds the size limit")]
    EndpointTooLong,
    #[error("delegated API endpoint must use HTTPS on port 443")]
    InsecureEndpoint,
    #[error("delegated API endpoint host is not allowed")]
    EndpointHostNotAllowed,
    #[error("delegated API app ID is invalid")]
    InvalidAppId,
    #[error("delegated API signing secret is invalid")]
    InvalidSigningSecret,
    #[error("delegated API credential is invalid")]
    InvalidJwt,
    #[error("delegated API credential is expired")]
    Expired,
    #[error("delegated API credential has insufficient lifetime")]
    InsufficientLifetime,
}

/// Runtime errors for delegated, read-only Qobuz API operations.
///
/// Network and response errors are intentionally sanitized: reqwest errors
/// may carry the delegated endpoint URL, which must not reach Debug/log output.
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum DelegatedApiError {
    #[error(transparent)]
    Configuration(#[from] DelegatedApiConfigError),
    #[error("delegated API credential is expired")]
    Expired,
    #[error("offline mode is active - delegated Qobuz services are disabled")]
    OfflineMode,
    #[error("delegated API HTTP client could not be initialized")]
    ClientInitialization,
    #[error("delegated API network request failed")]
    Network,
    #[error("delegated API credential was rejected")]
    Unauthorized,
    #[error("delegated API access was forbidden")]
    Forbidden,
    #[error("delegated API request signature was rejected")]
    InvalidSignature,
    #[error("delegated API returned an invalid response")]
    InvalidResponse,
    #[error("delegated API request was rejected (HTTP {0})")]
    RequestRejected(u16),
    #[error("delegated API was rate limited")]
    RateLimited,
    #[error("delegated API server error (HTTP {0})")]
    ServerError(u16),
    #[error("track {0} is no longer available on Qobuz")]
    TrackUnavailable(u64),
    #[error("no valid quality is available for this track")]
    NoQualityAvailable,
}

impl DelegatedApiError {
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            Self::Network | Self::RateLimited | Self::ServerError(_)
        )
    }
}

pub type DelegatedApiResult<T> = std::result::Result<T, DelegatedApiError>;

/// An HTTPS API base whose host has passed the caller's closed allowlist.
///
/// The allowlist belongs to the QConnect adapter because it is established by
/// official-client captures. Passing it explicitly prevents this lower-level
/// crate from silently broadening the accepted authority later.
pub struct DelegatedApiEndpoint {
    base_url: Zeroizing<String>,
}

impl DelegatedApiEndpoint {
    pub fn new(
        endpoint: &str,
        allowed_hosts: &[&str],
    ) -> std::result::Result<Self, DelegatedApiConfigError> {
        if endpoint.len() > MAX_ENDPOINT_BYTES {
            return Err(DelegatedApiConfigError::EndpointTooLong);
        }

        let mut base_url =
            Url::parse(endpoint).map_err(|_| DelegatedApiConfigError::InvalidEndpoint)?;
        if base_url.cannot_be_a_base()
            || !base_url.username().is_empty()
            || base_url.password().is_some()
            || base_url.query().is_some()
            || base_url.fragment().is_some()
        {
            return Err(DelegatedApiConfigError::InvalidEndpoint);
        }
        if base_url.scheme() != "https" || base_url.port_or_known_default() != Some(443) {
            return Err(DelegatedApiConfigError::InsecureEndpoint);
        }

        let host = base_url
            .host_str()
            .ok_or(DelegatedApiConfigError::InvalidEndpoint)?;
        if allowed_hosts.is_empty()
            || !allowed_hosts
                .iter()
                .any(|allowed| host.eq_ignore_ascii_case(allowed))
        {
            return Err(DelegatedApiConfigError::EndpointHostNotAllowed);
        }

        if !base_url.path().ends_with('/') {
            let normalized_path = format!("{}/", base_url.path());
            base_url.set_path(&normalized_path);
        }
        Ok(Self {
            base_url: Zeroizing::new(base_url.to_string()),
        })
    }

    fn join(&self, path: &str) -> DelegatedApiResult<Url> {
        Url::parse(self.base_url.as_str())
            .map_err(|_| {
                DelegatedApiError::Configuration(DelegatedApiConfigError::InvalidEndpoint)
            })?
            .join(path.trim_start_matches('/'))
            .map_err(|_| DelegatedApiError::Configuration(DelegatedApiConfigError::InvalidEndpoint))
    }
}

impl fmt::Debug for DelegatedApiEndpoint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DelegatedApiEndpoint")
            .field("base_url", &"<redacted>")
            .finish()
    }
}

/// App-level material copied into, and owned exclusively by, a guest client.
///
/// It intentionally does not implement `Clone`; the signing secret is wiped on
/// drop. Callers obtain this opaque value through
/// [`crate::QobuzClient::delegated_app_credentials`], which copies only the
/// app ID and the app secret selected by the owner's validation probe.
pub struct DelegatedAppCredentials {
    app_id: String,
    signing_secret: Zeroizing<String>,
}

impl DelegatedAppCredentials {
    pub(crate) fn new(
        app_id: String,
        signing_secret: String,
    ) -> std::result::Result<Self, DelegatedApiConfigError> {
        let signing_secret = Zeroizing::new(signing_secret);
        if app_id.is_empty()
            || app_id.len() > MAX_APP_ID_BYTES
            || app_id.chars().any(char::is_whitespace)
            || HeaderValue::from_str(&app_id).is_err()
        {
            return Err(DelegatedApiConfigError::InvalidAppId);
        }
        if signing_secret.is_empty()
            || signing_secret.len() > MAX_SIGNING_SECRET_BYTES
            || signing_secret.chars().any(char::is_whitespace)
        {
            return Err(DelegatedApiConfigError::InvalidSigningSecret);
        }
        Ok(Self {
            app_id,
            signing_secret,
        })
    }

    #[cfg(test)]
    pub(crate) fn values_for_test(&self) -> (&str, &str) {
        (&self.app_id, self.signing_secret.as_str())
    }
}

impl fmt::Debug for DelegatedAppCredentials {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DelegatedAppCredentials")
            .field("app_id", &"<redacted>")
            .field("signing_secret", &"<redacted>")
            .finish()
    }
}

/// Complete, in-memory configuration consumed when creating a guest client.
pub struct DelegatedApiConfig {
    endpoint: DelegatedApiEndpoint,
    expires_at: u64,
    jwt_api: Zeroizing<String>,
    app_credentials: DelegatedAppCredentials,
}

impl DelegatedApiConfig {
    pub fn new(
        endpoint: DelegatedApiEndpoint,
        expires_at: u64,
        jwt_api: String,
        app_credentials: DelegatedAppCredentials,
    ) -> std::result::Result<Self, DelegatedApiConfigError> {
        let jwt_api = Zeroizing::new(jwt_api);
        let now = unix_timestamp();
        if expires_at <= now {
            return Err(DelegatedApiConfigError::Expired);
        }
        if expires_at.saturating_sub(now) < MIN_INITIAL_TTL.as_secs() {
            return Err(DelegatedApiConfigError::InsufficientLifetime);
        }
        validate_jwt(&jwt_api)?;
        Ok(Self {
            endpoint,
            expires_at,
            jwt_api,
            app_credentials,
        })
    }
}

impl fmt::Debug for DelegatedApiConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DelegatedApiConfig")
            .field("endpoint", &self.endpoint)
            .field("expires_at", &self.expires_at)
            .field("jwt_api", &"<redacted>")
            .field("app_credentials", &self.app_credentials)
            .finish()
    }
}

/// A read-only Qobuz client whose authority is one delegated `jwt_api`.
///
/// The type is not clonable. Callers that need shared ownership can place the
/// whole isolated context in an `Arc`; this never shares state with the owner
/// [`crate::QobuzClient`].
pub struct DelegatedQobuzClient {
    http: Client,
    endpoint: DelegatedApiEndpoint,
    expires_at: u64,
    jwt_api: Zeroizing<String>,
    app_id: String,
    signing_secret: Zeroizing<String>,
}

impl DelegatedQobuzClient {
    pub fn new(config: DelegatedApiConfig) -> DelegatedApiResult<Self> {
        let http = qbz_net_proxy::apply_current(Client::builder())
            .map_err(|_| DelegatedApiError::ClientInitialization)?
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|_| DelegatedApiError::ClientInitialization)?;

        Ok(Self {
            http,
            endpoint: config.endpoint,
            expires_at: config.expires_at,
            jwt_api: config.jwt_api,
            app_id: config.app_credentials.app_id,
            signing_secret: config.app_credentials.signing_secret,
        })
    }

    pub fn expires_at(&self) -> u64 {
        self.expires_at
    }

    /// Prove the delegated Bearer against the observed non-mutating endpoint.
    pub async fn validate_access(&self) -> DelegatedApiResult<()> {
        let url = self.endpoint.join(paths::USER_GET)?;
        let response = self.execute(self.http.get(url)).await?;
        ensure_success(response.status(), None, false)?;
        let value: Value = read_json(response).await?;
        let valid_user_shape = value.as_object().is_some_and(|object| {
            object.contains_key("id")
                || object.contains_key("user")
                || object.contains_key("credential")
        });
        if !valid_user_shape {
            return Err(DelegatedApiError::InvalidResponse);
        }
        Ok(())
    }

    /// Resolve read-only metadata for one track with delegated authority.
    pub async fn get_track(&self, track_id: u64) -> DelegatedApiResult<Track> {
        let response = self
            .signed_get(
                paths::TRACK_GET,
                "trackget",
                &[("track_id", track_id.to_string())],
            )
            .await?;
        ensure_success(response.status(), Some(track_id), true)?;
        read_json(response).await
    }

    /// Resolve read-only metadata in the API's 50-track request windows.
    pub async fn get_tracks_batch(&self, track_ids: &[u64]) -> DelegatedApiResult<Vec<Track>> {
        if track_ids.is_empty() {
            return Ok(Vec::new());
        }

        let mut tracks = Vec::with_capacity(track_ids.len());
        for chunk in track_ids.chunks(TRACKS_PER_BATCH) {
            tracks.extend(self.get_tracks_batch_chunk(chunk).await?);
        }
        Ok(tracks)
    }

    /// Resolve one signed streaming URL using only the delegated Bearer.
    pub async fn get_stream_url(
        &self,
        track_id: u64,
        quality: Quality,
    ) -> DelegatedApiResult<StreamUrl> {
        let timestamp = get_timestamp();
        let track_id_param = track_id.to_string();
        let format_id = quality.id().to_string();
        let signing_params = [
            ("format_id", format_id.as_str()),
            ("intent", "stream"),
            ("track_id", track_id_param.as_str()),
        ];
        let signature = Zeroizing::new(delegated_signature(
            "trackgetFileUrl",
            &signing_params,
            timestamp,
            self.signing_secret.as_str(),
        ));
        let timestamp = timestamp.to_string();
        let url = self.endpoint.join(paths::TRACK_GET_FILE_URL)?;
        let response = self
            .execute(self.http.get(url).query(&[
                ("track_id", track_id_param.as_str()),
                ("format_id", format_id.as_str()),
                ("intent", "stream"),
                ("request_ts", timestamp.as_str()),
                ("request_sig", signature.as_str()),
            ]))
            .await?;
        ensure_success(response.status(), Some(track_id), true)?;

        let value: Value = read_json(response).await?;
        let restrictions: Vec<StreamRestriction> = value
            .get("restrictions")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|_| DelegatedApiError::InvalidResponse)?
            .unwrap_or_default();
        let stream_url = value
            .get("url")
            .and_then(Value::as_str)
            .filter(|url| !url.is_empty())
            .ok_or(DelegatedApiError::TrackUnavailable(track_id))?
            .to_string();

        Ok(StreamUrl {
            url: stream_url,
            format_id: value
                .get("format_id")
                .and_then(Value::as_u64)
                .unwrap_or_default() as u32,
            mime_type: value
                .get("mime_type")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            sampling_rate: value
                .get("sampling_rate")
                .and_then(Value::as_f64)
                .unwrap_or_default(),
            bit_depth: value
                .get("bit_depth")
                .and_then(Value::as_u64)
                .map(|depth| depth as u32),
            track_id,
            restrictions,
            sample: value
                .get("sample")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }

    /// Try the requested quality and lower tiers without ever switching to the
    /// owner session or refreshing the delegated credential.
    pub async fn get_stream_url_with_fallback(
        &self,
        track_id: u64,
        preferred: Quality,
    ) -> DelegatedApiResult<StreamUrl> {
        let qualities = Quality::fallback_order();
        let start = qualities
            .iter()
            .position(|quality| *quality == preferred)
            .unwrap_or_default();
        let mut unavailable = false;

        for quality in &qualities[start..] {
            match self.get_stream_url(track_id, *quality).await {
                Ok(stream) if !stream.has_restrictions() => return Ok(stream),
                Ok(_) => continue,
                Err(DelegatedApiError::TrackUnavailable(_)) => unavailable = true,
                Err(error) => return Err(error),
            }
        }

        if unavailable {
            Err(DelegatedApiError::TrackUnavailable(track_id))
        } else {
            Err(DelegatedApiError::NoQualityAvailable)
        }
    }

    async fn get_tracks_batch_chunk(&self, track_ids: &[u64]) -> DelegatedApiResult<Vec<Track>> {
        let ids = track_ids
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let timestamp = get_timestamp();
        let signature = Zeroizing::new(delegated_signature(
            "trackgetList",
            &[("tracks_id", ids.as_str())],
            timestamp,
            self.signing_secret.as_str(),
        ));
        let timestamp = timestamp.to_string();
        let url = self.endpoint.join(paths::TRACK_GET_LIST)?;
        let response = self
            .execute(
                self.http
                    .post(url)
                    .query(&[
                        ("request_ts", timestamp.as_str()),
                        ("request_sig", signature.as_str()),
                    ])
                    .json(&serde_json::json!({ "tracks_id": track_ids })),
            )
            .await?;
        ensure_success(response.status(), None, true)?;

        let value: Value = read_json(response).await?;
        let items = value
            .get("tracks")
            .and_then(|tracks| tracks.get("items"))
            .cloned()
            .ok_or(DelegatedApiError::InvalidResponse)?;
        serde_json::from_value(items).map_err(|_| DelegatedApiError::InvalidResponse)
    }

    async fn signed_get(
        &self,
        path: &str,
        method_name: &str,
        params: &[(&str, String)],
    ) -> DelegatedApiResult<Response> {
        let timestamp = get_timestamp();
        let signing_params = params
            .iter()
            .map(|(key, value)| (*key, value.as_str()))
            .collect::<Vec<_>>();
        let signature = Zeroizing::new(delegated_signature(
            method_name,
            &signing_params,
            timestamp,
            self.signing_secret.as_str(),
        ));
        let timestamp = timestamp.to_string();
        let mut query = signing_params;
        query.push(("request_ts", timestamp.as_str()));
        query.push(("request_sig", signature.as_str()));
        let url = self.endpoint.join(path)?;
        self.execute(self.http.get(url).query(&query)).await
    }

    async fn execute(&self, request: RequestBuilder) -> DelegatedApiResult<Response> {
        self.guard()?;
        request
            .headers(self.delegated_headers()?)
            .send()
            .await
            .map_err(|_| DelegatedApiError::Network)
    }

    fn guard(&self) -> DelegatedApiResult<()> {
        if crate::offline_gate::is_offline() {
            return Err(DelegatedApiError::OfflineMode);
        }
        if self.expires_at <= unix_timestamp() {
            return Err(DelegatedApiError::Expired);
        }
        Ok(())
    }

    fn delegated_headers(&self) -> DelegatedApiResult<HeaderMap> {
        let mut headers = HeaderMap::new();
        headers.insert(
            "X-App-Id",
            HeaderValue::from_str(&self.app_id).map_err(|_| {
                DelegatedApiError::Configuration(DelegatedApiConfigError::InvalidAppId)
            })?,
        );
        let mut authorization =
            bearer_header(self.jwt_api.as_str()).map_err(DelegatedApiError::Configuration)?;
        authorization.set_sensitive(true);
        headers.insert(AUTHORIZATION, authorization);
        Ok(headers)
    }
}

impl fmt::Debug for DelegatedQobuzClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DelegatedQobuzClient")
            .field("endpoint", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .field("jwt_api", &"<redacted>")
            .field("app_id", &"<redacted>")
            .field("signing_secret", &"<redacted>")
            .finish()
    }
}

fn validate_jwt(jwt: &str) -> std::result::Result<(), DelegatedApiConfigError> {
    if jwt.is_empty()
        || jwt.len() > MAX_JWT_BYTES
        || !jwt.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
    {
        return Err(DelegatedApiConfigError::InvalidJwt);
    }
    Ok(())
}

fn bearer_header(jwt: &str) -> std::result::Result<HeaderValue, DelegatedApiConfigError> {
    let mut bearer = Zeroizing::new(String::with_capacity("Bearer ".len() + jwt.len()));
    bearer.push_str("Bearer ");
    bearer.push_str(jwt);
    HeaderValue::from_str(bearer.as_str()).map_err(|_| DelegatedApiConfigError::InvalidJwt)
}

fn unix_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Sign without ever allocating a concatenated buffer containing the secret.
fn delegated_signature(
    method: &str,
    params: &[(&str, &str)],
    timestamp: u64,
    secret: &str,
) -> String {
    let mut sorted = params.to_vec();
    sorted.sort_by_key(|(key, _)| *key);
    let mut hasher = Md5::new();
    hasher.update(method.as_bytes());
    for (key, value) in sorted {
        hasher.update(key.as_bytes());
        hasher.update(value.as_bytes());
    }
    hasher.update(timestamp.to_string().as_bytes());
    hasher.update(secret.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn ensure_success(
    status: StatusCode,
    track_id: Option<u64>,
    signed: bool,
) -> DelegatedApiResult<()> {
    match status {
        status if status.is_success() => Ok(()),
        StatusCode::BAD_REQUEST if signed => Err(DelegatedApiError::InvalidSignature),
        StatusCode::UNAUTHORIZED => Err(DelegatedApiError::Unauthorized),
        StatusCode::FORBIDDEN => Err(DelegatedApiError::Forbidden),
        StatusCode::NOT_FOUND => track_id.map_or_else(
            || Err(DelegatedApiError::RequestRejected(status.as_u16())),
            |track_id| Err(DelegatedApiError::TrackUnavailable(track_id)),
        ),
        StatusCode::TOO_MANY_REQUESTS => Err(DelegatedApiError::RateLimited),
        status if status.is_server_error() => Err(DelegatedApiError::ServerError(status.as_u16())),
        status => Err(DelegatedApiError::RequestRejected(status.as_u16())),
    }
}

async fn read_json<T: DeserializeOwned>(response: Response) -> DelegatedApiResult<T> {
    let body = response
        .bytes()
        .await
        .map_err(|_| DelegatedApiError::Network)?;
    serde_json::from_slice(&body).map_err(|_| DelegatedApiError::InvalidResponse)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Once;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    const APP_ID: &str = "delegated-app-id";
    const SIGNING_SECRET: &str = "delegated-signing-secret";
    const JWT: &str = "delegated.jwt.value";

    fn install_tls_provider() {
        static INSTALL: Once = Once::new();
        INSTALL.call_once(|| {
            let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        });
    }

    fn future_expiry() -> u64 {
        unix_timestamp() + 600
    }

    fn test_client(base_url: String) -> DelegatedQobuzClient {
        install_tls_provider();
        let endpoint = DelegatedApiEndpoint {
            base_url: Zeroizing::new(base_url),
        };
        let credentials =
            DelegatedAppCredentials::new(APP_ID.to_string(), SIGNING_SECRET.to_string()).unwrap();
        let config =
            DelegatedApiConfig::new(endpoint, future_expiry(), JWT.to_string(), credentials)
                .unwrap();
        DelegatedQobuzClient::new(config).unwrap()
    }

    async fn mock_once(body: &'static str) -> (String, tokio::task::JoinHandle<String>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut chunk = [0_u8; 1024];
            loop {
                let read = socket.read(&mut chunk).await.unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            socket.write_all(response.as_bytes()).await.unwrap();
            String::from_utf8(request).unwrap()
        });
        (format!("http://{address}/api.json/0.2/"), task)
    }

    fn request_headers(request: &str) -> BTreeMap<String, String> {
        request
            .lines()
            .skip(1)
            .take_while(|line| !line.is_empty())
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_string()))
            .collect()
    }

    #[test]
    fn endpoint_requires_https_and_a_caller_allowlist() {
        let allowed = ["api.example.test"];
        assert!(matches!(
            DelegatedApiEndpoint::new("http://api.example.test/v1", &allowed),
            Err(DelegatedApiConfigError::InsecureEndpoint)
        ));
        assert!(matches!(
            DelegatedApiEndpoint::new("https://other.example.test/v1", &allowed),
            Err(DelegatedApiConfigError::EndpointHostNotAllowed)
        ));
        assert!(matches!(
            DelegatedApiEndpoint::new("https://user@api.example.test/v1", &allowed),
            Err(DelegatedApiConfigError::InvalidEndpoint)
        ));
        assert!(matches!(
            DelegatedApiEndpoint::new("https://api.example.test/v1?token=nope", &allowed),
            Err(DelegatedApiConfigError::InvalidEndpoint)
        ));

        let endpoint = DelegatedApiEndpoint::new("https://api.example.test/v1", &allowed).unwrap();
        assert_eq!(
            endpoint.join("track/get").unwrap().as_str(),
            "https://api.example.test/v1/track/get"
        );
    }

    #[test]
    fn config_rejects_expired_short_lived_and_invalid_credentials() {
        let make_config = |expires_at, jwt: &str| {
            let endpoint =
                DelegatedApiEndpoint::new("https://api.example.test/v1", &["api.example.test"])
                    .unwrap();
            let credentials =
                DelegatedAppCredentials::new(APP_ID.to_string(), SIGNING_SECRET.to_string())
                    .unwrap();
            DelegatedApiConfig::new(endpoint, expires_at, jwt.to_string(), credentials)
        };
        let now = unix_timestamp();
        assert!(matches!(
            make_config(now, JWT),
            Err(DelegatedApiConfigError::Expired)
        ));
        assert!(matches!(
            make_config(now + 59, JWT),
            Err(DelegatedApiConfigError::InsufficientLifetime)
        ));
        assert!(matches!(
            make_config(now + 600, "delegated jwt with spaces"),
            Err(DelegatedApiConfigError::InvalidJwt)
        ));
    }

    #[test]
    fn secrets_and_authorization_never_appear_in_debug() {
        install_tls_provider();
        let endpoint_text = "https://api.example.test/private/base";
        let endpoint = DelegatedApiEndpoint::new(endpoint_text, &["api.example.test"]).unwrap();
        let credentials =
            DelegatedAppCredentials::new(APP_ID.to_string(), SIGNING_SECRET.to_string()).unwrap();
        let config =
            DelegatedApiConfig::new(endpoint, future_expiry(), JWT.to_string(), credentials)
                .unwrap();
        let config_debug = format!("{config:?}");
        for forbidden in [
            endpoint_text,
            APP_ID,
            SIGNING_SECRET,
            JWT,
            "Authorization",
            "Bearer",
        ] {
            assert!(!config_debug.contains(forbidden));
        }

        let client = DelegatedQobuzClient::new(config).unwrap();
        let client_debug = format!("{client:?}");
        for forbidden in [
            endpoint_text,
            APP_ID,
            SIGNING_SECRET,
            JWT,
            "Authorization",
            "Bearer",
        ] {
            assert!(!client_debug.contains(forbidden));
        }

        let headers_debug = format!("{:?}", client.delegated_headers().unwrap());
        assert!(headers_debug.contains("Sensitive"));
        assert!(!headers_debug.contains(JWT));
        assert!(!headers_debug.contains("Bearer"));
        assert!(!headers_debug.contains("x-user-auth-token"));
    }

    #[tokio::test]
    async fn access_preflight_uses_only_bearer_and_app_id() {
        let _lock = crate::offline_gate::test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _reset = crate::offline_gate::TestGateReset;
        crate::offline_gate::set_offline(false);
        let (base_url, server) = mock_once(r#"{"id": 7}"#).await;
        let client = test_client(base_url);
        client.validate_access().await.unwrap();
        let request = server.await.unwrap();
        let request_line = request.lines().next().unwrap();
        let headers = request_headers(&request);

        assert_eq!(request_line, "GET /api.json/0.2/user/get HTTP/1.1");
        assert_eq!(headers.get("x-app-id").map(String::as_str), Some(APP_ID));
        assert_eq!(
            headers.get("authorization").map(String::as_str),
            Some("Bearer delegated.jwt.value")
        );
        assert!(!headers.contains_key("x-user-auth-token"));
    }

    #[tokio::test]
    async fn metadata_request_keeps_delegated_headers_and_is_signed() {
        let _lock = crate::offline_gate::test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _reset = crate::offline_gate::TestGateReset;
        crate::offline_gate::set_offline(false);
        let (base_url, server) = mock_once(r#"{"id": 42}"#).await;
        let client = test_client(base_url);
        let track = client.get_track(42).await.unwrap();
        assert_eq!(track.id, 42);

        let request = server.await.unwrap();
        let request_line = request.lines().next().unwrap();
        let headers = request_headers(&request);
        assert_eq!(headers.get("x-app-id").map(String::as_str), Some(APP_ID));
        assert_eq!(
            headers.get("authorization").map(String::as_str),
            Some("Bearer delegated.jwt.value")
        );
        assert!(!headers.contains_key("x-user-auth-token"));

        let target = request_line.split_whitespace().nth(1).unwrap();
        let url = Url::parse(&format!("http://localhost{target}")).unwrap();
        let query = url.query_pairs().into_owned().collect::<BTreeMap<_, _>>();
        assert_eq!(url.path(), "/api.json/0.2/track/get");
        assert_eq!(query.get("track_id").map(String::as_str), Some("42"));
        let timestamp = query.get("request_ts").unwrap().parse::<u64>().unwrap();
        let expected =
            crate::auth::sign_request("trackget", &[("track_id", "42")], timestamp, SIGNING_SECRET);
        assert_eq!(query.get("request_sig"), Some(&expected));
    }

    #[test]
    fn expired_credentials_fail_before_any_request() {
        let _lock = crate::offline_gate::test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _reset = crate::offline_gate::TestGateReset;
        crate::offline_gate::set_offline(false);
        let mut client = test_client("http://127.0.0.1:9/api.json/0.2/".to_string());
        client.expires_at = 0;
        assert_eq!(client.guard(), Err(DelegatedApiError::Expired));
    }
}
