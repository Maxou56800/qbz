//! Qobuz API client implementation

use reqwest::{Client, StatusCode};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::RwLock;

use super::auth::{
    get_timestamp, parse_login_response, sign_file_url, sign_get_favorites, sign_get_file_url,
    sign_request, sign_search, sign_session_start,
};
use super::bundle::{self, BundleTokens};
use super::delegated::{DelegatedApiConfigError, DelegatedAppCredentials};
use super::endpoints::{self, paths};
use super::error::{ApiError, Result};
use super::forbidden_breaker::ForbiddenBreaker;
use super::lyrics::{
    merge_translation_into, QobuzLyricsContent, QobuzLyricsDocument, QobuzLyricsUrls,
};
use qbz_models::*;
use zeroize::Zeroizing;

/// Only the token-restoration POST; shared streaming/download requests remain unbounded.
const TOKEN_RESTORE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

const USER_AGENT: &str = "Mozilla/5.0 (X11; Linux x86_64; rv:120.0) Gecko/20100101 Firefox/120.0";

/// Return a bounded, content-free diagnostic for an unexpected response body.
///
/// Remote bodies are untrusted and can contain credentials or echo signed
/// request parameters. They must never enter logs or a displayable `ApiError`.
/// We retain only whether a body was present.
async fn body_preview(response: reqwest::Response) -> String {
    match response.text().await {
        Ok(body) => {
            let body = Zeroizing::new(body);
            redacted_body_diagnostic(body.as_str()).to_string()
        }
        Err(_) => String::new(),
    }
}

fn redacted_body_diagnostic(body: &str) -> &'static str {
    if body.trim().is_empty() {
        " : <empty body>"
    } else {
        " : <response body omitted>"
    }
}

/// CMAF session state (session/start + infos for key derivation)
struct CmafSession {
    session_id: String,
    infos: String,
    expires_at: u64,
}

/// The app secret which has actually passed the API probe, together with the
/// app ID of the bundle generation it belongs to.
///
/// Keeping the provenance beside the secret prevents a background bundle
/// rotation from silently pairing a previously validated secret with a new
/// app ID. The secret is app-level material, never a user session token, and
/// is wiped when replaced or when the last client owner is dropped.
struct ValidatedBundleSecret {
    app_id: String,
    signing_secret: Zeroizing<String>,
}

impl ValidatedBundleSecret {
    fn new(app_id: String, signing_secret: String) -> Self {
        Self {
            app_id,
            signing_secret: Zeroizing::new(signing_secret),
        }
    }

    fn is_current_for(&self, tokens: &BundleTokens) -> bool {
        self.app_id == tokens.app_id
            && tokens
                .secrets
                .iter()
                .any(|candidate| candidate == self.signing_secret.as_str())
    }

    fn copy_material(&self) -> (String, String) {
        (self.app_id.clone(), self.signing_secret.to_string())
    }
}

/// Qobuz API client
pub struct QobuzClient {
    http: Client,
    tokens: Arc<RwLock<Option<BundleTokens>>>,
    session: Arc<RwLock<Option<UserSession>>>,
    validated_secret: Arc<RwLock<Option<ValidatedBundleSecret>>>,
    locale: Arc<RwLock<String>>,
    cmaf_session: Arc<RwLock<Option<CmafSession>>>,
    /// Where the regenerable bundle-token cache goes, when the embedder rather
    /// than the platform decides. `None` keeps `dirs::cache_dir()`.
    bundle_cache_dir: Option<PathBuf>,
    /// Backs off the hot streaming/favorites paths after repeated 403s so a
    /// post-outage account hiccup can't be escalated into a per-IP edge block
    /// by the no-backoff prefetch scheduler (issue #637).
    forbidden_breaker: Arc<ForbiddenBreaker>,
}

impl Clone for QobuzClient {
    fn clone(&self) -> Self {
        Self {
            http: self.http.clone(),
            tokens: Arc::clone(&self.tokens),
            session: Arc::clone(&self.session),
            validated_secret: Arc::clone(&self.validated_secret),
            locale: Arc::clone(&self.locale),
            cmaf_session: Arc::clone(&self.cmaf_session),
            bundle_cache_dir: self.bundle_cache_dir.clone(),
            forbidden_breaker: Arc::clone(&self.forbidden_breaker),
        }
    }
}

impl QobuzClient {
    /// Create a new client, caching bundle tokens under the platform cache
    /// directory.
    pub fn new() -> Result<Self> {
        Self::build(None)
    }

    /// Create a client whose regenerable bundle-token cache lives beneath
    /// `cache_dir`, for hosts that are handed their cache location rather than
    /// deriving it from the environment.
    pub fn with_cache_dir(cache_dir: PathBuf) -> Result<Self> {
        Self::build(Some(cache_dir))
    }

    fn build(bundle_cache_dir: Option<PathBuf>) -> Result<Self> {
        let http = Client::builder()
            .user_agent(USER_AGENT)
            .cookie_store(true)
            // Bound the TCP connect phase so a dead route (e.g. a stale CDN
            // address) can't hang startup. Does not affect body-read time, so
            // long streaming reads are unaffected.
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()?;

        Ok(Self {
            http,
            tokens: Arc::new(RwLock::new(None)),
            session: Arc::new(RwLock::new(None)),
            validated_secret: Arc::new(RwLock::new(None)),
            locale: Arc::new(RwLock::new("en".to_string())),
            cmaf_session: Arc::new(RwLock::new(None)),
            bundle_cache_dir,
            forbidden_breaker: Arc::new(ForbiddenBreaker::new()),
        })
    }

    // === 403 circuit breaker (issue #637) ===

    /// Short-circuit an authenticated request when the 403 breaker is open.
    /// Returns `Err(ForbiddenCircuitOpen)` — no network is touched — so a
    /// post-outage 403 storm cannot get the user's IP edge-blocked. Callers put
    /// this at the top of the hot streaming/favorites paths.
    fn forbidden_guard(&self) -> Result<()> {
        // Test hook: `QBZ_FORCE_403=1` forces the breaker open so the whole 403
        // back-off path (no-network short-circuit + abort-fallback + the audible
        // "backing off" toast) can be smoke-tested on a HEALTHY account, which
        // otherwise can't reproduce the incident. Off by default; issue #637.
        if std::env::var_os("QBZ_FORCE_403").is_some() {
            return Err(ApiError::ForbiddenCircuitOpen(30));
        }
        if let Some(remaining) = self.forbidden_breaker.blocked_for() {
            return Err(ApiError::ForbiddenCircuitOpen(remaining.as_secs()));
        }
        Ok(())
    }

    /// Feed an authenticated response's status to the breaker: a 403 counts
    /// toward opening it; any success resets it. Other statuses are neutral
    /// (they have their own handling and must not open the breaker).
    fn note_forbidden_status(&self, status: StatusCode) {
        if status == StatusCode::FORBIDDEN {
            if let Some(cooldown) = self.forbidden_breaker.record_forbidden() {
                log::warn!(
                    "[403-breaker] Repeated 403s from Qobuz — backing off for {}s (no network) \
                     to avoid an edge/IP block. See issue #637.",
                    cooldown.as_secs()
                );
            }
        } else if status.is_success() {
            self.forbidden_breaker.record_success();
        }
    }

    /// Initialize client by extracting bundle tokens.
    ///
    /// Warm start: if cached tokens exist, use them immediately so the UI never
    /// blocks on Qobuz's (sometimes very slow) ~7 MB bundle download, then
    /// refresh in the background — re-downloading only if Qobuz rotated the
    /// bundle version. Cold start (first run or after a cache wipe): fetch now,
    /// bounded by a per-request timeout + a small retry so a slow/dead CDN can't
    /// hang forever.
    ///
    /// Returns `true` if it served cached tokens (warm), `false` if it had to do
    /// a live extraction (cold) — callers can use this to drive a "connecting"
    /// UI only when it actually matters.
    pub async fn init(&self) -> Result<bool> {
        if let Some(cached) = bundle::load_cached_bundle_from(self.bundle_cache_dir.as_deref()) {
            let version = cached.bundle_version.clone();
            log::info!("[Bundle] Using cached tokens (version {})", version);
            *self.tokens.write().await = Some(cached.into());
            *self.validated_secret.write().await = None;

            // Cache reads are never gated, but the background refresh is a
            // network request — gate it once before cloning the client into
            // the spawned task, skipping the refresh instead of failing the
            // warm start.
            match self.http() {
                Ok(client) => {
                    let client = client.clone();
                    let tokens_arc = Arc::clone(&self.tokens);
                    let validated_secret = Arc::clone(&self.validated_secret);
                    let cache_dir = self.bundle_cache_dir.clone();
                    tokio::spawn(async move {
                        if let Some(fresh) = bundle::refresh_bundle_if_changed_in(
                            &client,
                            &version,
                            cache_dir.as_deref(),
                        )
                        .await
                        {
                            *tokens_arc.write().await = Some(fresh);
                            *validated_secret.write().await = None;
                            log::info!("[Bundle] Background refresh applied rotated tokens");
                        }
                    });
                }
                Err(_) => {
                    log::info!("[Bundle] Offline mode - skipping background bundle refresh");
                }
            }
            return Ok(true);
        }

        log::info!("[Bundle] No cached tokens, extracting from Qobuz...");
        // Cold start: a live bundle fetch is a network request — gated on
        // purpose so an offline cold start fails fast instead of waiting out
        // the network timeouts.
        let tokens = bundle::extract_and_cache_bundle_tokens_in(
            self.http()?,
            self.bundle_cache_dir.as_deref(),
        )
        .await?;
        *self.tokens.write().await = Some(tokens);
        *self.validated_secret.write().await = None;
        Ok(false)
    }

    /// Set the locale for API requests
    pub async fn set_locale(&self, locale: String) {
        *self.locale.write().await = locale;
    }

    /// Get the current locale (public for cache key generation)
    pub async fn get_locale(&self) -> String {
        self.locale.read().await.clone()
    }

    /// Get the current locale (internal use)
    async fn locale(&self) -> String {
        self.locale.read().await.clone()
    }

    /// Get app ID (public for catalog search)
    pub async fn app_id(&self) -> Result<String> {
        self.tokens
            .read()
            .await
            .as_ref()
            .map(|t| t.app_id.clone())
            .ok_or_else(|| ApiError::BundleExtractionError("Client not initialized".to_string()))
    }

    /// Get HTTP client reference (public for catalog search)
    pub fn get_http(&self) -> &Client {
        &self.http
    }

    /// The single offline choke point (D3): every Qobuz SERVICE request flows
    /// through here. While offline mode is active, fail fast with a typed,
    /// non-transient error instead of timing out against the network.
    ///
    /// Exemption: the sign-in methods (`login`, `login_with_oauth_code`,
    /// `login_with_token`) use the raw `self.http` field instead —
    /// user-initiated authentication is an explicit intent to reach Qobuz;
    /// the offline gate governs services, not sign-in. Without the exemption
    /// a closed gate (induced offline, or a stale offline session) refuses
    /// the very login that would resolve it.
    pub(crate) fn http(&self) -> Result<&Client> {
        if crate::offline_gate::is_offline() {
            return Err(ApiError::OfflineMode);
        }
        Ok(&self.http)
    }

    /// Return app ID and signing secret from one validated bundle generation.
    async fn validated_app_material(&self) -> Result<(String, String)> {
        let (app_id, candidates) = {
            let tokens = self.tokens.read().await;
            let tokens = tokens.as_ref().ok_or_else(|| {
                ApiError::BundleExtractionError("Client not initialized".to_string())
            })?;

            if let Some(validated) = self.validated_secret.read().await.as_ref() {
                if validated.is_current_for(tokens) {
                    return Ok(validated.copy_material());
                }
            }

            (
                tokens.app_id.clone(),
                Zeroizing::new(tokens.secrets.clone()),
            )
        };

        for secret in candidates.iter() {
            if self.test_secret(secret).await? {
                // A background refresh is allowed while the network probe is
                // running. Accept its result only if the exact app material is
                // still present in the current bundle generation.
                let tokens = self.tokens.read().await;
                let current = tokens.as_ref().ok_or_else(|| {
                    ApiError::BundleExtractionError("Client not initialized".to_string())
                })?;
                if current.app_id != app_id
                    || !current.secrets.iter().any(|candidate| candidate == secret)
                {
                    return Err(ApiError::BundleExtractionError(
                        "Bundle credentials changed during validation".to_string(),
                    ));
                }

                let validated = ValidatedBundleSecret::new(app_id.clone(), secret.clone());
                let material = validated.copy_material();
                *self.validated_secret.write().await = Some(validated);
                return Ok(material);
            }
        }

        Err(ApiError::InvalidAppSecret)
    }

    /// Get the validated signing secret (validates bundle candidates on first use).
    pub(crate) async fn secret(&self) -> Result<String> {
        self.validated_app_material()
            .await
            .map(|(_, signing_secret)| signing_secret)
    }

    /// Copy only validated app-level material into an isolated QConnect guest.
    ///
    /// The returned value does not contain, share, or copy this client's
    /// [`UserSession`], user auth token, OAuth private key, cookies, or HTTP
    /// client. In particular, it uses the secret selected by the existing API
    /// validation probe; it never guesses by taking the first bundle secret.
    pub async fn delegated_app_credentials(&self) -> Result<DelegatedAppCredentials> {
        let (app_id, signing_secret) = self.validated_app_material().await?;
        DelegatedAppCredentials::new(app_id, signing_secret).map_err(|error| match error {
            DelegatedApiConfigError::InvalidAppId => ApiError::InvalidAppId,
            _ => ApiError::InvalidAppSecret,
        })
    }

    /// Test if a secret is valid using a known track
    async fn test_secret(&self, secret: &str) -> Result<bool> {
        let test_track_id = 5966783u64; // Known test track
        let timestamp = get_timestamp();
        let signature = sign_get_file_url(test_track_id, 5, timestamp, secret);

        let url = endpoints::build_url(paths::TRACK_GET_FILE_URL);
        let response = self
            .http()?
            .get(&url)
            .headers(self.api_headers().await?)
            .query(&[
                ("track_id", test_track_id.to_string()),
                ("format_id", "5".to_string()),
                ("intent", "stream".to_string()),
                ("request_ts", timestamp.to_string()),
                ("request_sig", signature),
            ])
            .send()
            .await?;

        Ok(response.status() != StatusCode::BAD_REQUEST)
    }

    /// Login with email and password
    pub async fn login(&self, email: &str, password: &str) -> Result<UserSession> {
        let url = endpoints::build_url(paths::USER_LOGIN);
        // Auth exemption: raw client, bypasses the offline gate (sign-in is
        // explicit user intent to reach Qobuz; the gate governs services).
        let response = self
            .http
            .get(&url)
            .headers(self.api_headers().await?)
            .query(&[("email", email), ("password", password)])
            .send()
            .await?;

        match response.status() {
            StatusCode::OK => {
                let json: Value = response.json().await?;
                let session = parse_login_response(&json)?;
                *self.session.write().await = Some(session.clone());
                Ok(session)
            }
            StatusCode::UNAUTHORIZED => Err(ApiError::AuthenticationError(
                "Invalid credentials".to_string(),
            )),
            StatusCode::BAD_REQUEST => Err(ApiError::InvalidAppId),
            status => Err(ApiError::ApiResponse(format!(
                "Unexpected status: {}",
                status
            ))),
        }
    }

    /// Raw `POST /user/login` with an OAuth `user_auth_token`: HTTP status
    /// and body, verbatim. Same request as [`Self::login_with_token`], but it
    /// never parses the payload and never stores a session, so an account
    /// the parser rejects today (`IneligibleUser`) can still be captured for
    /// the redacted fixtures in `qbz-nix-docs/qobuz-api/`. Probe support
    /// only (`examples/member_login_probe.rs`).
    pub async fn login_with_token_raw(&self, token: &str) -> Result<(u16, String)> {
        use reqwest::header::{HeaderMap, HeaderValue};

        let app_id = self.app_id().await?;
        let mut headers = HeaderMap::new();
        headers.insert(
            "X-App-Id",
            HeaderValue::from_str(&app_id).map_err(|_| ApiError::InvalidAppId)?,
        );
        headers.insert(
            "X-User-Auth-Token",
            HeaderValue::from_str(token)
                .map_err(|_| ApiError::AuthenticationError("Invalid token format".into()))?,
        );
        let response = self
            .http
            .post(endpoints::build_url(endpoints::paths::USER_LOGIN))
            .headers(headers)
            .header("Content-Type", "text/plain;charset=UTF-8")
            .body("extra=partner")
            .send()
            .await?;
        let status = response.status().as_u16();
        let body = response.text().await?;
        Ok((status, body))
    }

    /// Check if logged in
    pub async fn is_logged_in(&self) -> bool {
        self.session.read().await.is_some()
    }

    /// Logout - clear the session
    pub async fn logout(&self) {
        *self.session.write().await = None;
    }

    /// Inject an already-authenticated session (e.g. from OAuth flow).
    /// Use this when the session was obtained outside this client instance.
    pub async fn set_session(&self, session: UserSession) {
        *self.session.write().await = Some(session);
    }

    /// Account language (ISO 639-1) of the active session, if the login
    /// response carried one — the "Auto" target for lyrics translation.
    /// `None` pre-login and for pre-v10 persisted sessions.
    pub async fn session_language_code(&self) -> Option<String> {
        self.session
            .read()
            .await
            .as_ref()
            .and_then(|s| s.language_code.clone())
    }

    /// A copy of the active session (entitlements included), `None` before
    /// login. The shared session lifecycle reads the verdict from here.
    pub async fn session(&self) -> Option<UserSession> {
        self.session.read().await.clone()
    }

    /// Exchange an OAuth code for a full user session.
    ///
    /// This implements the new Qobuz OAuth flow:
    /// OAuth step 1 on its own: `GET /oauth/callback?code&private_key&app_id`
    /// → the `user_auth_token`. Nothing is stored; `login_with_oauth_code`
    /// is this plus the `/user/login` session fetch, and the member-mode
    /// probe uses it alone so it can capture that second response raw.
    pub async fn exchange_oauth_code(&self, code: &str) -> Result<String> {
        use reqwest::header::{HeaderMap, HeaderValue};

        let (app_id, private_key) = {
            let guard = self.tokens.read().await;
            let tokens = guard.as_ref().ok_or_else(|| {
                ApiError::BundleExtractionError("Client not initialized".to_string())
            })?;
            let private_key = tokens.private_key.clone().ok_or_else(|| {
                ApiError::BundleExtractionError(
                    "OAuth private key not available in bundle".to_string(),
                )
            })?;
            (tokens.app_id.clone(), private_key)
        };

        let callback_url = endpoints::build_url(endpoints::paths::OAUTH_CALLBACK);
        let mut headers = HeaderMap::new();
        headers.insert(
            "X-App-Id",
            HeaderValue::from_str(&app_id).map_err(|_| ApiError::InvalidAppId)?,
        );

        log::info!("[OAuth] Exchanging code for token via /oauth/callback");
        // Auth exemption: raw client, bypasses the offline gate (sign-in is
        // explicit user intent to reach Qobuz; the gate governs services).
        let callback_response = self
            .http
            .get(&callback_url)
            .headers(headers)
            .query(&[
                ("code", code),
                ("private_key", &private_key),
                ("app_id", &app_id),
            ])
            .send()
            .await?;

        if !callback_response.status().is_success() {
            return Err(ApiError::ApiResponse(format!(
                "OAuth callback failed with status {}",
                callback_response.status()
            )));
        }

        let callback_json: serde_json::Value = callback_response.json().await?;
        callback_json["token"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| {
                ApiError::ApiResponse("OAuth callback: no token in response".to_string())
            })
    }

    /// 1. GET /oauth/callback?code=CODE&private_key=KEY → { token }
    /// 2. POST /user/login with X-User-Auth-Token: token, body=extra=partner → UserSession
    pub async fn login_with_oauth_code(&self, code: &str) -> Result<UserSession> {
        use reqwest::header::{HeaderMap, HeaderValue};

        let app_id = self.app_id().await?;

        // Step 1: Exchange code for token
        let token = self.exchange_oauth_code(code).await?;

        log::info!("[OAuth] Got token, fetching user session via /user/login");

        // Step 2: Fetch user session using the token
        let user_login_url = endpoints::build_url(endpoints::paths::USER_LOGIN);
        let mut auth_headers = HeaderMap::new();
        auth_headers.insert(
            "X-App-Id",
            HeaderValue::from_str(&app_id).map_err(|_| ApiError::InvalidAppId)?,
        );
        auth_headers.insert(
            "X-User-Auth-Token",
            HeaderValue::from_str(&token)
                .map_err(|_| ApiError::AuthenticationError("Invalid OAuth token format".into()))?,
        );

        // Auth exemption: raw client (see /oauth/callback step above).
        let login_response = self
            .http
            .post(&user_login_url)
            .headers(auth_headers)
            .header("Content-Type", "text/plain;charset=UTF-8")
            .body("extra=partner")
            .send()
            .await?;

        match login_response.status() {
            StatusCode::OK => {
                let json: serde_json::Value = login_response.json().await?;
                let session = super::auth::parse_login_response(&json)?;
                *self.session.write().await = Some(session.clone());
                log::info!("[OAuth] Session established");
                Ok(session)
            }
            StatusCode::UNAUTHORIZED => Err(ApiError::AuthenticationError(
                "OAuth token rejected by user/login".to_string(),
            )),
            status => Err(ApiError::ApiResponse(format!(
                "user/login OAuth step failed with status {}",
                status
            ))),
        }
    }

    /// Restore a session from a previously saved OAuth user_auth_token.
    ///
    /// Used at startup when the user logged in via OAuth (web browser) and the
    /// token was persisted. Calls POST /user/login with X-User-Auth-Token header.
    /// Returns an error if the token has expired.
    pub async fn login_with_token(&self, token: &str) -> Result<UserSession> {
        let tokens = self.tokens.read().await;
        let app_id = tokens
            .as_ref()
            .ok_or_else(|| ApiError::BundleExtractionError("Client not initialized".to_string()))?
            .app_id
            .clone();
        drop(tokens);

        let request = self.token_restore_request(
            &endpoints::build_url(endpoints::paths::USER_LOGIN),
            &app_id,
            token,
        )?;
        self.complete_token_restore(request).await
    }

    /// Private request seam: tests substitute a loopback URL and override the
    /// request's duration. Production always uses USER_LOGIN and 15 seconds.
    fn token_restore_request(
        &self,
        url: &str,
        app_id: &str,
        token: &str,
    ) -> Result<reqwest::RequestBuilder> {
        use reqwest::header::{HeaderMap, HeaderValue};
        let mut headers = HeaderMap::new();
        headers.insert(
            "X-App-Id",
            HeaderValue::from_str(&app_id).map_err(|_| ApiError::InvalidAppId)?,
        );
        headers.insert(
            "X-User-Auth-Token",
            HeaderValue::from_str(token)
                .map_err(|_| ApiError::AuthenticationError("Invalid token format".into()))?,
        );

        // Reqwest carries this SAME deadline through headers AND body reads,
        // including a body that keeps trickling bytes. No timeout surrounds
        // session publication or the caller's subsequent store activation.
        Ok(self
            .http
            .post(url)
            .headers(headers)
            .header("Content-Type", "text/plain;charset=UTF-8")
            .body("extra=partner")
            .timeout(TOKEN_RESTORE_TIMEOUT))
    }

    async fn complete_token_restore(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<UserSession> {
        log::info!("[OAuth] Restoring session from saved token");
        // Auth exemption: raw client bypasses the offline service gate.
        let resp = request.send().await?;

        match resp.status() {
            StatusCode::OK => {
                let json: serde_json::Value = resp.json().await?;
                let session = super::auth::parse_login_response(&json)?;
                *self.session.write().await = Some(session.clone());
                log::info!("[OAuth] Session restored from token");
                Ok(session)
            }
            StatusCode::UNAUTHORIZED => Err(ApiError::AuthenticationError(
                "OAuth token expired or invalid".to_string(),
            )),
            status => Err(ApiError::ApiResponse(format!(
                "Token re-auth failed: {}",
                status
            ))),
        }
    }

    /// Get current user info (display name, subscription, and expiry if available)
    pub async fn get_user_info(&self) -> Option<(String, String, Option<String>)> {
        self.session.read().await.as_ref().map(|s| {
            (
                s.display_name.clone(),
                s.subscription_label.clone(),
                s.subscription_valid_until.clone(),
            )
        })
    }

    /// Get user auth token header value (public for catalog search)
    pub async fn auth_token(&self) -> Result<String> {
        self.session
            .read()
            .await
            .as_ref()
            .map(|s| s.user_auth_token.clone())
            .ok_or_else(|| ApiError::AuthenticationError("Not logged in".to_string()))
    }

    // === Header helpers ===

    /// Build standard API headers.
    /// Always includes X-App-Id. Includes X-User-Auth-Token when logged in.
    async fn api_headers(&self) -> Result<reqwest::header::HeaderMap> {
        use reqwest::header::{HeaderMap, HeaderValue};
        let mut headers = HeaderMap::new();

        let app_id = self.app_id().await?;
        headers.insert(
            "X-App-Id",
            HeaderValue::from_str(&app_id).map_err(|_| ApiError::InvalidAppId)?,
        );

        if let Ok(token) = self.auth_token().await {
            if let Ok(val) = HeaderValue::from_str(&token) {
                headers.insert("X-User-Auth-Token", val);
            }
        }

        Ok(headers)
    }

    /// Build headers that REQUIRE authentication. Fails if not logged in.
    pub(crate) async fn authenticated_headers(&self) -> Result<reqwest::header::HeaderMap> {
        use reqwest::header::{HeaderMap, HeaderValue};
        let mut headers = HeaderMap::new();

        let app_id = self.app_id().await?;
        headers.insert(
            "X-App-Id",
            HeaderValue::from_str(&app_id).map_err(|_| ApiError::InvalidAppId)?,
        );

        let token = self.auth_token().await?;
        headers.insert(
            "X-User-Auth-Token",
            HeaderValue::from_str(&token)
                .map_err(|_| ApiError::AuthenticationError("Invalid auth token format".into()))?,
        );

        Ok(headers)
    }

    /// Build a signed GET request. Computes request_sig from the endpoint method name
    /// and query params, then appends request_ts + request_sig to the query.
    /// `method_name` is the endpoint path without slashes, e.g. "albumget".
    async fn signed_get(
        &self,
        url: &str,
        method_name: &str,
        params: &[(&str, String)],
    ) -> Result<reqwest::Response> {
        let timestamp = get_timestamp();
        let secret = self.secret().await?;
        let kv: Vec<(&str, &str)> = params.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let sig = sign_request(method_name, &kv, timestamp, &secret);
        let ts_str = timestamp.to_string();

        let mut query_params: Vec<(&str, &str)> = kv;
        query_params.push(("request_ts", &ts_str));
        query_params.push(("request_sig", &sig));

        let response = self
            .http()?
            .get(url)
            .headers(self.api_headers().await?)
            .query(&query_params)
            .send()
            .await?;
        Ok(response)
    }

    /// Same as signed_get but uses authenticated headers (requires login).
    async fn signed_get_auth(
        &self,
        url: &str,
        method_name: &str,
        params: &[(&str, String)],
    ) -> Result<reqwest::Response> {
        let timestamp = get_timestamp();
        let secret = self.secret().await?;
        let kv: Vec<(&str, &str)> = params.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let sig = sign_request(method_name, &kv, timestamp, &secret);
        let ts_str = timestamp.to_string();

        let mut query_params: Vec<(&str, &str)> = kv;
        query_params.push(("request_ts", &ts_str));
        query_params.push(("request_sig", &sig));

        let response = self
            .http()?
            .get(url)
            .headers(self.authenticated_headers().await?)
            .query(&query_params)
            .send()
            .await?;
        Ok(response)
    }

    // === Search endpoints ===

    /// Search for albums
    /// Optional search_type: "MainArtist", "Performer", "Composer", "Label", "ReleaseName"
    pub async fn search_albums(
        &self,
        query: &str,
        limit: u32,
        offset: u32,
        search_type: Option<&str>,
    ) -> Result<SearchResultsPage<Album>> {
        let url = endpoints::build_url(paths::ALBUM_SEARCH);
        let timestamp = get_timestamp();
        let secret = self.secret().await?;
        let signature = sign_search(
            "albumsearch",
            query,
            limit,
            offset,
            search_type,
            timestamp,
            &secret,
        );
        let limit_str = limit.to_string();
        let offset_str = offset.to_string();
        let ts_str = timestamp.to_string();

        let mut params: Vec<(&str, &str)> = vec![
            ("query", query),
            ("limit", &limit_str),
            ("offset", &offset_str),
            ("request_ts", &ts_str),
            ("request_sig", &signature),
        ];

        if let Some(st) = search_type {
            params.push(("type", st));
        }

        let http_response = self
            .http()?
            .get(&url)
            .headers(self.api_headers().await?)
            .query(&params)
            .send()
            .await?;
        log::debug!("[API] search_albums status={}", http_response.status());
        let response: Value = http_response.json().await?;

        let albums = response
            .get("albums")
            .ok_or_else(|| ApiError::ApiResponse("No albums in response".to_string()))?;

        Ok(serde_json::from_value(albums.clone())?)
    }

    /// Search for tracks
    /// Optional search_type: "MainArtist", "Performer", "Composer", "Label", "ReleaseName"
    pub async fn search_tracks(
        &self,
        query: &str,
        limit: u32,
        offset: u32,
        search_type: Option<&str>,
    ) -> Result<SearchResultsPage<Track>> {
        let url = endpoints::build_url(paths::TRACK_SEARCH);
        let timestamp = get_timestamp();
        let secret = self.secret().await?;
        let signature = sign_search(
            "tracksearch",
            query,
            limit,
            offset,
            search_type,
            timestamp,
            &secret,
        );
        let limit_str = limit.to_string();
        let offset_str = offset.to_string();
        let ts_str = timestamp.to_string();

        let mut params: Vec<(&str, &str)> = vec![
            ("query", query),
            ("limit", &limit_str),
            ("offset", &offset_str),
            ("request_ts", &ts_str),
            ("request_sig", &signature),
        ];

        if let Some(st) = search_type {
            params.push(("type", st));
        }

        let http_response = self
            .http()?
            .get(&url)
            .headers(self.api_headers().await?)
            .query(&params)
            .send()
            .await?;
        log::debug!("[API] search_tracks status={}", http_response.status());
        let response: Value = http_response.json().await?;

        let tracks = response
            .get("tracks")
            .ok_or_else(|| ApiError::ApiResponse("No tracks in response".to_string()))?;

        Ok(serde_json::from_value(tracks.clone())?)
    }

    /// Search for artists
    /// Optional search_type: "MainArtist", "Performer", "Composer", "Label", "ReleaseName"
    pub async fn search_artists(
        &self,
        query: &str,
        limit: u32,
        offset: u32,
        search_type: Option<&str>,
    ) -> Result<SearchResultsPage<Artist>> {
        let url = endpoints::build_url(paths::ARTIST_SEARCH);
        let timestamp = get_timestamp();
        let secret = self.secret().await?;
        let signature = sign_search(
            "artistsearch",
            query,
            limit,
            offset,
            search_type,
            timestamp,
            &secret,
        );
        let limit_str = limit.to_string();
        let offset_str = offset.to_string();
        let ts_str = timestamp.to_string();

        let mut params: Vec<(&str, &str)> = vec![
            ("query", query),
            ("limit", &limit_str),
            ("offset", &offset_str),
            ("request_ts", &ts_str),
            ("request_sig", &signature),
        ];

        if let Some(st) = search_type {
            params.push(("type", st));
        }

        let http_response = self
            .http()?
            .get(&url)
            .headers(self.api_headers().await?)
            .query(&params)
            .send()
            .await?;
        log::debug!("[API] search_artists status={}", http_response.status());
        let response: Value = http_response.json().await?;

        let artists = response
            .get("artists")
            .ok_or_else(|| ApiError::ApiResponse("No artists in response".to_string()))?;

        Ok(serde_json::from_value(artists.clone())?)
    }

    /// Catalog search (combined: albums, tracks, artists, playlists, most_popular).
    /// Returns raw JSON for caller to parse — the response shape is complex.
    pub async fn catalog_search(&self, query: &str, limit: u32, offset: u32) -> Result<Value> {
        let url = endpoints::build_url(paths::CATALOG_SEARCH);
        let timestamp = get_timestamp();
        let secret = self.secret().await?;
        let signature = sign_search(
            "catalogsearch",
            query,
            limit,
            offset,
            None,
            timestamp,
            &secret,
        );
        let limit_str = limit.to_string();
        let offset_str = offset.to_string();
        let ts_str = timestamp.to_string();

        let http_response = self
            .http()?
            .get(&url)
            .headers(self.api_headers().await?)
            .query(&[
                ("query", query),
                ("limit", &limit_str),
                ("offset", &offset_str),
                ("request_ts", &ts_str),
                ("request_sig", &signature),
            ])
            .send()
            .await?;
        log::debug!("[API] catalog_search status={}", http_response.status());
        let response: Value = http_response.json().await?;
        Ok(response)
    }

    /// Get similar artists for an artist ID
    pub async fn get_similar_artists(
        &self,
        artist_id: u64,
        limit: u32,
        offset: u32,
    ) -> Result<SearchResultsPage<Artist>> {
        let url = endpoints::build_url(paths::ARTIST_GET_SIMILAR);
        let http_response = self
            .signed_get(
                &url,
                "artistgetSimilarArtists",
                &[
                    ("artist_id", artist_id.to_string()),
                    ("limit", limit.to_string()),
                    ("offset", offset.to_string()),
                ],
            )
            .await?;
        log::debug!(
            "[API] get_similar_artists({}) status={}",
            artist_id,
            http_response.status()
        );
        let response: Value = http_response.json().await?;

        let artists = response
            .get("artists")
            .ok_or_else(|| ApiError::ApiResponse("No artists in response".to_string()))?;

        Ok(serde_json::from_value(artists.clone())?)
    }

    /// Get an artist's tracks (public endpoint via artist/get?extra=tracks)
    pub async fn get_artist_tracks(
        &self,
        artist_id: u64,
        limit: u32,
        offset: u32,
    ) -> Result<TracksContainer> {
        let url = endpoints::build_url(paths::ARTIST_GET);
        let locale = self.locale().await;

        let http_response = self
            .signed_get(
                &url,
                "artistget",
                &[
                    ("artist_id", artist_id.to_string()),
                    ("extra", "tracks".to_string()),
                    ("lang", locale),
                    ("limit", limit.to_string()),
                    ("offset", offset.to_string()),
                ],
            )
            .await?;
        log::debug!(
            "[API] get_artist_tracks({}) status={}",
            artist_id,
            http_response.status()
        );
        let response: Value = http_response.json().await?;

        let tracks = response
            .get("tracks")
            .ok_or_else(|| ApiError::ApiResponse("No tracks in artist response".to_string()))?;

        Ok(serde_json::from_value(tracks.clone())?)
    }

    // === Get endpoints ===

    /// Get album by ID
    pub async fn get_album(&self, album_id: &str) -> Result<Album> {
        let url = endpoints::build_url(paths::ALBUM_GET);
        let http_response = self
            .signed_get(&url, "albumget", &[("album_id", album_id.to_string())])
            .await?;
        let status = http_response.status();
        log::debug!("[API] get_album({}) status={}", album_id, status);

        if status == StatusCode::NOT_FOUND {
            log::warn!(
                "[API] get_album({}) returned 404 — album not found",
                album_id
            );
            return Err(ApiError::ApiResponse(format!(
                "Album {} not found (404)",
                album_id
            )));
        }
        if !status.is_success() {
            log::error!("[API] get_album({}) unexpected status={}", album_id, status);
            return Err(ApiError::ApiResponse(format!(
                "get_album({}) status {}",
                album_id, status
            )));
        }

        let response: Value = http_response.json().await?;
        Ok(serde_json::from_value(response)?)
    }

    /// Get featured albums by type (new-releases, press-awards, most-streamed)
    pub async fn get_featured_albums(
        &self,
        featured_type: &str,
        limit: u32,
        offset: u32,
        genre_id: Option<u64>,
    ) -> Result<SearchResultsPage<Album>> {
        let url = endpoints::build_url(paths::ALBUM_GET_FEATURED);
        let mut query = vec![
            ("type".to_string(), featured_type.to_string()),
            ("limit".to_string(), limit.to_string()),
            ("offset".to_string(), offset.to_string()),
        ];

        if let Some(gid) = genre_id {
            query.push(("genre_id".to_string(), gid.to_string()));
        }

        let params: Vec<(&str, String)> =
            query.iter().map(|(k, v)| (k.as_str(), v.clone())).collect();
        let http_response = self.signed_get(&url, "albumgetFeatured", &params).await?;
        log::debug!(
            "[API] get_featured_albums({}) status={}",
            featured_type,
            http_response.status()
        );
        let response: Value = http_response.json().await?;

        let albums = response
            .get("albums")
            .ok_or_else(|| ApiError::ApiResponse("No albums in response".to_string()))?;

        Ok(serde_json::from_value(albums.clone())?)
    }

    /// Get Release Watch — new releases from artists, labels or awards the
    /// user follows. Qobuz mobile surfaces this as "Radar de Novedades" /
    /// "Release Watch". Endpoint and signature confirmed in
    /// `qbz-nix-docs/qobuz-api-inferred-openapi-v9.7.0.3.yaml`.
    ///
    /// `release_type` is required and must be one of `artists`, `labels`,
    /// `awards` (matches the three tabs in the mobile UI).
    pub async fn get_release_watch(
        &self,
        release_type: &str,
        limit: u32,
        offset: u32,
    ) -> Result<SearchResultsPage<Album>> {
        let url = endpoints::build_url(paths::FAVORITE_GET_NEW_RELEASES);
        let limit_str = limit.to_string();
        let offset_str = offset.to_string();

        let http_response = self
            .http()?
            .get(&url)
            .headers(self.authenticated_headers().await?)
            .query(&[
                ("type", release_type),
                ("limit", limit_str.as_str()),
                ("offset", offset_str.as_str()),
            ])
            .send()
            .await?;

        let status = http_response.status();
        log::info!(
            "[API] get_release_watch(type={}, limit={}, offset={}) status={}",
            release_type,
            limit,
            offset,
            status
        );

        let body_text = http_response.text().await?;
        if !status.is_success() {
            log::warn!(
                "[API] get_release_watch non-success body (first 400 chars): {}",
                body_text.chars().take(400).collect::<String>()
            );
            return Err(ApiError::ApiResponse(format!(
                "get_release_watch status {}",
                status
            )));
        }

        let response: Value = serde_json::from_str(&body_text)?;

        // Shape is V2GenericListDto<AlbumDto> per x20/b.java — a thin
        // pagination envelope: {has_more: bool, items: [...]}. No total/
        // offset/limit fields are returned. We only need items for the UI
        // and the caller already knows the offset/limit it asked for, so
        // we project onto SearchResultsPage<Album> to stay compatible with
        // the rest of the album-list plumbing.
        //
        // Note on shape: AlbumDto carries both `artist` (singular, legacy)
        // and `artists` (array, current). `/favorite/getNewReleases` tends
        // to omit `artist` and only populate `artists[]`, which made our
        // `Album` struct deserialize an empty `artist` and the UI show
        // "Unknown Artist". Backfill `artist` from `artists[0]` before
        // parsing so the existing deserializer just works.
        let mut items_value = response.get("items").cloned().unwrap_or(Value::Null);
        if let Some(items_arr) = items_value.as_array_mut() {
            for item in items_arr {
                if let Some(obj) = item.as_object_mut() {
                    let needs_backfill = obj.get("artist").map(|v| v.is_null()).unwrap_or(true);
                    if needs_backfill {
                        if let Some(first_artist) = obj
                            .get("artists")
                            .and_then(|a| a.as_array())
                            .and_then(|arr| arr.first())
                            .cloned()
                        {
                            obj.insert("artist".to_string(), first_artist);
                        }
                    }
                }
            }
        }
        let items: Vec<Album> = serde_json::from_value(items_value).map_err(|e| {
            log::warn!(
                "[API] get_release_watch items parse error: {}. Body (first 600 chars): {}",
                e,
                body_text.chars().take(600).collect::<String>()
            );
            ApiError::ApiResponse(format!("get_release_watch items parse: {}", e))
        })?;

        let has_more = response
            .get("has_more")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let total_hint = if has_more {
            // We don't know the real total; signal "at least one more page"
            // by bumping past the items we have.
            offset + items.len() as u32 + 1
        } else {
            offset + items.len() as u32
        };

        Ok(SearchResultsPage::<Album> {
            items,
            total: total_hint,
            offset,
            limit,
        })
    }

    /// Albums similar to a seed album (`/album/suggest`). Ported from
    /// the legacy api client.
    pub async fn get_album_suggest(&self, album_id: &str) -> Result<AlbumSuggestResponse> {
        let url = endpoints::build_url(paths::ALBUM_SUGGEST);
        let http_response = self
            .http()?
            .get(&url)
            .headers(self.api_headers().await?)
            .query(&[("album_id", album_id)])
            .send()
            .await?;
        let status = http_response.status();
        if !status.is_success() {
            return Err(ApiError::ApiResponse(format!(
                "get_album_suggest({album_id}) status {status}"
            )));
        }
        let response: Value = http_response.json().await?;
        Ok(serde_json::from_value(response)?)
    }

    /// Dynamic suggestions for mixes (`POST /dynamic/suggest`). Seeds
    /// from recently-listened track ids. Returns the suggested tracks
    /// (items parsed leniently). Ported from the legacy api client;
    /// the `track_to_analysed` payload is optional and omitted here.
    pub async fn get_dynamic_suggest(
        &self,
        listened_track_ids: &[u64],
        limit: u32,
    ) -> Result<Vec<Track>> {
        self.get_dynamic_suggest_full(listened_track_ids, &[], limit)
            .await
    }

    /// Like [`get_dynamic_suggest`] but carrying the `track_to_analysed`
    /// payload — the PRIMARY DailyQ/WeeklyQ path. Tauri seeds this with up to 9
    /// resolved `{track_id, artist_id, genre_id, label_id}` tuples and only
    /// falls back to an empty analysis when a call returns zero items.
    pub async fn get_dynamic_suggest_full(
        &self,
        listened_track_ids: &[u64],
        tracks_to_analyse: &[TrackToAnalyse],
        limit: u32,
    ) -> Result<Vec<Track>> {
        let url = endpoints::build_url(paths::DYNAMIC_SUGGEST);
        let body = serde_json::json!({
            "limit": limit,
            "listened_tracks_ids": listened_track_ids,
            "track_to_analysed": tracks_to_analyse,
        });
        let http_response = self
            .http()?
            .post(&url)
            .headers(self.authenticated_headers().await?)
            .json(&body)
            .send()
            .await?;
        let status = http_response.status();
        if !status.is_success() {
            return Err(ApiError::ApiResponse(format!(
                "get_dynamic_suggest status {status}"
            )));
        }
        let response: Value = http_response.json().await?;
        Ok(qbz_models::lenient::parse_items_array(
            &response,
            "tracks",
            "dynamic-suggest track",
        ))
    }

    /// Qobuz radio for an artist (`/radio/artist`) — a generated track
    /// list. Ported from the legacy api client.
    pub async fn get_radio_artist(&self, artist_id: &str) -> Result<RadioResponse> {
        self.get_radio(paths::RADIO_ARTIST, "artist_id", artist_id)
            .await
    }

    /// Qobuz radio for a track (`/radio/track`).
    pub async fn get_radio_track(&self, track_id: &str) -> Result<RadioResponse> {
        self.get_radio(paths::RADIO_TRACK, "track_id", track_id)
            .await
    }

    /// Qobuz radio for an album (`/radio/album`).
    pub async fn get_radio_album(&self, album_id: &str) -> Result<RadioResponse> {
        self.get_radio(paths::RADIO_ALBUM, "album_id", album_id)
            .await
    }

    async fn get_radio(&self, path: &str, key: &str, id: &str) -> Result<RadioResponse> {
        let url = endpoints::build_url(path);
        let http_response = self
            .http()?
            .get(&url)
            .headers(self.authenticated_headers().await?)
            .query(&[(key, id)])
            .send()
            .await?;
        let status = http_response.status();
        if !status.is_success() {
            return Err(ApiError::ApiResponse(format!(
                "get_radio({path}, {id}) status {status}"
            )));
        }
        let response: Value = http_response.json().await?;
        Ok(serde_json::from_value(response)?)
    }

    /// Get list of genres
    pub async fn get_genres(&self, parent_id: Option<u64>) -> Result<Vec<GenreInfo>> {
        let url = endpoints::build_url(paths::GENRE_LIST);
        // Force English for consistent genre names across all user regions
        let mut query: Vec<(&str, String)> = vec![("lang", "en".to_string())];

        if let Some(pid) = parent_id {
            query.push(("parent_id", pid.to_string()));
        }

        let http_response = self
            .signed_get(
                &url,
                "genrelist",
                &query
                    .iter()
                    .map(|(k, v)| (*k, v.clone()))
                    .collect::<Vec<_>>(),
            )
            .await?;
        log::debug!(
            "[API] get_genres(parent={:?}) status={}",
            parent_id,
            http_response.status()
        );
        let response: Value = http_response.json().await?;

        let genres = response
            .get("genres")
            .and_then(|g| g.get("items"))
            .ok_or_else(|| ApiError::ApiResponse("No genres in response".to_string()))?;

        Ok(serde_json::from_value(genres.clone())?)
    }

    /// Get discover index (home page content: playlists, ideal discography, etc.)
    pub async fn get_discover_index(
        &self,
        genre_ids: Option<Vec<u64>>,
    ) -> Result<DiscoverResponse> {
        let url = endpoints::build_url(paths::DISCOVER_INDEX);
        let mut query: Vec<(&str, String)> = vec![];

        // Add genre_ids as comma-separated list if provided
        if let Some(gids) = genre_ids {
            if !gids.is_empty() {
                let ids_str = gids
                    .iter()
                    .map(|id| id.to_string())
                    .collect::<Vec<_>>()
                    .join(",");
                query.push(("genre_ids", ids_str));
            }
        }

        let http_response = self
            .signed_get_auth(
                &url,
                "discoverindex",
                &query
                    .iter()
                    .map(|(k, v)| (*k, v.clone()))
                    .collect::<Vec<_>>(),
            )
            .await?;
        log::info!(
            "[API] get_discover_index genre_ids={:?} status={}",
            query,
            http_response.status()
        );
        let response: Value = http_response.json().await?;

        // Debug: log the response structure
        if let Some(obj) = response.as_object() {
            log::info!(
                "Discover API response keys: {:?}",
                obj.keys().collect::<Vec<_>>()
            );
            if let Some(err) = obj.get("message") {
                log::error!("Discover API error: {:?}", err);
            }
            if let Some(code) = obj.get("code") {
                log::error!("Discover API error code: {:?}", code);
            }
        }

        Ok(serde_json::from_value(response)?)
    }

    /// Get discover albums from a specific browse endpoint (newReleases, idealDiscography, mostStreamed)
    pub async fn get_discover_albums(
        &self,
        endpoint: &str,
        genre_ids: Option<Vec<u64>>,
        offset: u32,
        limit: u32,
    ) -> Result<DiscoverData<DiscoverAlbum>> {
        let url = endpoints::build_url(endpoint);
        let mut query: Vec<(&str, String)> = vec![];

        if let Some(gids) = genre_ids {
            if !gids.is_empty() {
                let ids_str = gids
                    .iter()
                    .map(|id| id.to_string())
                    .collect::<Vec<_>>()
                    .join(",");
                query.push(("genre_ids", ids_str));
            }
        }

        query.push(("offset", offset.to_string()));
        query.push(("limit", limit.to_string()));

        // Derive method name from endpoint path: "/discover/newReleases" -> "discovernewReleases"
        let method_name = endpoint.replace('/', "").replace('.', "");
        let http_response = self
            .signed_get_auth(
                &url,
                &method_name,
                &query
                    .iter()
                    .map(|(k, v)| (*k, v.clone()))
                    .collect::<Vec<_>>(),
            )
            .await?;
        log::info!(
            "[API] get_discover_albums({}) query={:?} status={}",
            endpoint,
            query,
            http_response.status()
        );
        let response: serde_json::Value = http_response.json().await?;

        Ok(serde_json::from_value(response)?)
    }

    /// Get discover playlists with optional tag and genre filters
    /// Example: tags=label, genre_ids=112,119
    pub async fn get_discover_playlists(
        &self,
        tag: Option<String>,
        genre_ids: Option<Vec<u64>>,
        limit: Option<u32>,
        offset: Option<u32>,
    ) -> Result<DiscoverPlaylistsResponse> {
        let url = endpoints::build_url(paths::DISCOVER_PLAYLISTS);
        let mut query: Vec<(&str, String)> = vec![];

        // Add tag filter if provided (e.g., "label", "partner")
        if let Some(ref t) = tag {
            query.push(("tags", t.clone()));
        }

        // Add genre_ids as comma-separated list if provided
        if let Some(gids) = genre_ids {
            if !gids.is_empty() {
                let ids_str = gids
                    .iter()
                    .map(|id| id.to_string())
                    .collect::<Vec<_>>()
                    .join(",");
                query.push(("genre_ids", ids_str));
            }
        }

        // Add limit (default 20)
        let lim = limit.unwrap_or(20);
        query.push(("limit", lim.to_string()));

        // Add offset (default 0)
        let off = offset.unwrap_or(0);
        query.push(("offset", off.to_string()));

        log::debug!(
            "[API] get_discover_playlists URL: {} query: {:?}",
            url,
            query
        );

        // First get raw JSON to debug structure
        let raw_response: serde_json::Value = self
            .signed_get_auth(
                &url,
                "discoverplaylists",
                &query
                    .iter()
                    .map(|(k, v)| (*k, v.clone()))
                    .collect::<Vec<_>>(),
            )
            .await?
            .json()
            .await?;

        log::debug!(
            "[API] get_discover_playlists raw response keys: {:?}",
            raw_response
                .as_object()
                .map(|o| o.keys().collect::<Vec<_>>())
        );

        // Try to parse as expected structure
        let response: DiscoverPlaylistsResponse = serde_json::from_value(raw_response.clone())
            .map_err(|e| {
                log::error!("[API] Failed to parse discover playlists response: {}", e);
                log::error!(
                    "[API] Raw response: {}",
                    serde_json::to_string_pretty(&raw_response).unwrap_or_default()
                );
                e
            })?;

        log::debug!(
            "[API] get_discover_playlists response: {} playlists",
            response.items.len()
        );

        Ok(response)
    }

    /// Get playlist tags with localized names
    pub async fn get_playlist_tags(&self) -> Result<Vec<PlaylistTag>> {
        let url = endpoints::build_url(paths::PLAYLIST_GET_TAGS);

        let http_response = self.signed_get_auth(&url, "playlistgetTags", &[]).await?;
        log::info!("[API] get_playlist_tags status={}", http_response.status());

        let raw: PlaylistTagsResponse = http_response.json().await?;

        // Get current locale (e.g., "en", "es", "fr", "de")
        let locale = self.locale().await;
        let lang = locale.split('-').next().unwrap_or("en");

        // Convert raw tags to PlaylistTag with localized name
        let tags: Vec<PlaylistTag> = raw
            .tags
            .into_iter()
            .filter(|tag| tag.is_discover.as_deref() == Some("true"))
            .filter_map(|tag| {
                // Parse name_json to get localized name
                let name_map: serde_json::Value = serde_json::from_str(&tag.name_json).ok()?;
                let name = name_map
                    .get(lang)
                    .or_else(|| name_map.get("en"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())?;
                let id = tag
                    .featured_tag_id
                    .as_ref()
                    .and_then(|s| s.parse::<u64>().ok())
                    .unwrap_or(0);
                Some(PlaylistTag {
                    id,
                    slug: tag.slug,
                    name,
                })
            })
            .collect();

        log::debug!("[API] get_playlist_tags: {} tags", tags.len());
        Ok(tags)
    }

    /// Get track by ID
    pub async fn get_track(&self, track_id: u64) -> Result<Track> {
        let url = endpoints::build_url(paths::TRACK_GET);
        let http_response = self
            .signed_get(&url, "trackget", &[("track_id", track_id.to_string())])
            .await?;
        let status = http_response.status();
        log::debug!("[API] get_track({}) status={}", track_id, status);

        if status == StatusCode::NOT_FOUND {
            log::warn!(
                "[API] get_track({}) returned 404 — track no longer available",
                track_id
            );
            return Err(ApiError::TrackUnavailable(track_id));
        }
        if !status.is_success() {
            log::error!("[API] get_track({}) unexpected status={}", track_id, status);
            return Err(ApiError::ApiResponse(format!(
                "get_track({}) status {}",
                track_id, status
            )));
        }

        let response: Value = http_response.json().await?;
        Ok(serde_json::from_value(response)?)
    }

    // === Lyrics (v9.9.0.0-beta delta) ===

    /// Raw step-1 lyrics call: signed `GET /track/lyricsUrl`, returning
    /// `(HTTP status, raw body)`. The signed-GET helpers are private to the
    /// client, so this is the only way diagnostic tooling (e.g.
    /// `examples/lyrics_probe.rs`) can observe the exact wire shape; the
    /// typed [`Self::get_lyrics_url`] builds on it.
    ///
    /// `language` (ISO 639-1, API v10) requests a translation; it is appended
    /// to the signed param list ONLY when `Some` (`sign_request` sorts the
    /// kv pairs alphabetically, so signing needs no change). Pass `None` for
    /// the original-only fetch.
    pub async fn get_lyrics_url_raw(
        &self,
        track_id: u64,
        language: Option<&str>,
    ) -> Result<(u16, String)> {
        let url = endpoints::build_url(paths::TRACK_LYRICS_URL);
        // Signing method name per the concatenation convention ("trackget" at
        // get_track, "trackgetFileUrl" in auth.rs): path with slashes removed,
        // case preserved -> "tracklyricsUrl". Verified against the live API
        // 2026-06-10 (HTTP 200 with a valid envelope).
        let mut params = vec![("track_id", track_id.to_string())];
        if let Some(language) = language {
            params.push(("language", language.to_string()));
        }
        let response = self
            .signed_get_auth(&url, "tracklyricsUrl", &params)
            .await?;
        let status = response.status().as_u16();
        let body = response.text().await?;
        Ok((status, body))
    }

    /// Step 1: fetch the lyrics-URL envelope for a track.
    ///
    /// `Ok(None)` is the typed miss: no lyrics for this track (non-200, or a
    /// 200 envelope without a usable `lyrics_url`, or an unparsable body).
    /// `Err` is reserved for transport/auth/offline failures — the lyrics
    /// orchestrator degrades those to a miss as well (spec §1.1); nothing in
    /// this path is ever a user-facing error.
    pub async fn get_lyrics_url(
        &self,
        track_id: u64,
        language: Option<&str>,
    ) -> Result<Option<QobuzLyricsUrls>> {
        let (status, body) = self.get_lyrics_url_raw(track_id, language).await?;
        if status != 200 {
            // Live-observed miss: HTTP 404 with body
            // {"status":"error","code":404,"message":"No lyrics found"}
            // (sample: qbz-nix-docs/qobuz-api/lyrics-miss-response.json).
            log::debug!(
                "[API] get_lyrics_url({}) status={} -> no lyrics",
                track_id,
                status
            );
            return Ok(None);
        }
        match serde_json::from_str::<QobuzLyricsUrls>(&body) {
            Ok(urls) if urls.lyrics_url.is_some() => Ok(Some(urls)),
            Ok(_) => {
                log::debug!(
                    "[API] get_lyrics_url({}) 200 without lyrics_url -> no lyrics",
                    track_id
                );
                Ok(None)
            }
            Err(e) => {
                log::warn!(
                    "[API] get_lyrics_url({}) unparsable 200 body ({}) -> no lyrics",
                    track_id,
                    e
                );
                Ok(None)
            }
        }
    }

    /// Steps 1 + 2: fetch the full lyrics document for a track.
    ///
    /// The CDN GET goes through [`Self::http`] (the offline choke point) so
    /// offline mode fails fast. The returned `lyrics_url` is self-authorizing
    /// — a pre-signed CloudFront URL (`Expires`/`Signature`/`Key-Pair-Id`);
    /// no `X-App-Id`/auth headers are sent (verified live 2026-06-10, HTTP
    /// 200 on a bare GET). `Ok(None)` = typed miss, same contract as
    /// [`Self::get_lyrics_url`]; a document without usable content counts as
    /// a miss.
    ///
    /// `language` (ISO 639-1, API v10) requests a translation; `None` fetches
    /// original-only (the default flow). LIVE WIRE (verified 2026-07-22):
    /// when a translation is requested AND available, `lyrics_url` points at
    /// the TRANSLATED document (`<id>-<lang>.json`), which carries ONLY
    /// `translation` and no `original` — so this fn then fetches the
    /// original-only document too and merges both into one bilingual
    /// document. When requested but NOT available the signed CDN URL 403s →
    /// typed miss (fail soft). When the document has an `original` but does
    /// not EMBED a `translation` block, the envelope's
    /// `translation_requested.lyrics_url` is fetched as a fallback and its
    /// content becomes the translation (fail soft: any gap in that fallback
    /// just leaves the document original-only).
    pub async fn get_lyrics(
        &self,
        track_id: u64,
        language: Option<&str>,
    ) -> Result<Option<QobuzLyricsDocument>> {
        let doc = match self.fetch_lyrics_doc(track_id, language).await? {
            Some(doc) => doc,
            None => return Ok(None),
        };

        if language.is_some() && doc.original.is_none() && doc.translation.is_some() {
            // Translation-only document (the common v10 live shape): fetch the
            // original document separately and merge into one bilingual doc.
            return match self.fetch_lyrics_doc(track_id, None).await? {
                Some(mut original_doc) if original_doc.original.is_some() => {
                    merge_translation_into(&mut original_doc, doc);
                    Ok(Some(original_doc))
                }
                _ => {
                    log::debug!(
                        "[API] get_lyrics({}) translation found but original doc missing -> no lyrics",
                        track_id
                    );
                    Ok(None)
                }
            };
        }

        // Preserve the original-only miss semantics: a document with neither
        // `original` nor a mergeable `translation` is unusable.
        if doc.original.is_none() {
            log::debug!(
                "[API] get_lyrics({}) document without original content -> no lyrics",
                track_id
            );
            return Ok(None);
        }
        Ok(Some(doc))
    }

    /// One lyrics round trip (signed lyricsUrl + bare CDN GET). Accepts any
    /// document carrying `original` OR `translation` content; the caller
    /// decides which shapes are usable (see [`Self::get_lyrics`]).
    async fn fetch_lyrics_doc(
        &self,
        track_id: u64,
        language: Option<&str>,
    ) -> Result<Option<QobuzLyricsDocument>> {
        let urls = match self.get_lyrics_url(track_id, language).await? {
            Some(urls) => urls,
            None => return Ok(None),
        };
        let lyrics_url = match urls.lyrics_url.clone() {
            Some(url) => url,
            None => return Ok(None),
        };

        let response = self.http()?.get(&lyrics_url).send().await?;
        let status = response.status();
        if !status.is_success() {
            log::debug!(
                "[API] get_lyrics({}) CDN doc status={} -> no lyrics",
                track_id,
                status
            );
            return Ok(None);
        }
        let body = response.text().await?;
        match serde_json::from_str::<QobuzLyricsDocument>(&body) {
            Ok(mut doc) if doc.original.is_some() || doc.translation.is_some() => {
                if language.is_some() && doc.original.is_some() && doc.translation.is_none() {
                    doc.translation = self.fetch_translation_fallback(track_id, &urls).await;
                }
                Ok(Some(doc))
            }
            Ok(_) => {
                log::debug!(
                    "[API] get_lyrics({}) document without any lyrics content -> no lyrics",
                    track_id
                );
                Ok(None)
            }
            Err(e) => {
                log::warn!(
                    "[API] get_lyrics({}) unparsable CDN doc ({}) -> no lyrics",
                    track_id,
                    e
                );
                Ok(None)
            }
        }
    }

    /// Fallback translation source (spec §B.3): when the main document did
    /// not embed a `translation` block, fetch the document the envelope's
    /// `translation_requested.lyrics_url` points at (same bare CDN GET) and
    /// take its content as the translation. Every failure mode degrades to
    /// `None` — translation is best-effort, never an error.
    async fn fetch_translation_fallback(
        &self,
        track_id: u64,
        urls: &QobuzLyricsUrls,
    ) -> Option<QobuzLyricsContent> {
        let translation_url = urls
            .translation_requested
            .as_ref()
            .and_then(|t| t.lyrics_url.as_deref())?;
        let response = self.http().ok()?.get(translation_url).send().await.ok()?;
        if !response.status().is_success() {
            log::debug!(
                "[API] get_lyrics({}) translation doc status={} -> original only",
                track_id,
                response.status()
            );
            return None;
        }
        let body = response.text().await.ok()?;
        match serde_json::from_str::<QobuzLyricsDocument>(&body) {
            // The translation document carries its content under `original`
            // (same wrapper shape as the main document).
            Ok(doc) => doc.original,
            Err(e) => {
                log::debug!(
                    "[API] get_lyrics({}) unparsable translation doc ({}) -> original only",
                    track_id,
                    e
                );
                None
            }
        }
    }

    /// Get artist by ID (basic info only - no albums, faster response)
    pub async fn get_artist_basic(&self, artist_id: u64) -> Result<Artist> {
        let url = endpoints::build_url(paths::ARTIST_GET);
        let locale = self.locale().await;
        let query = vec![
            ("artist_id", artist_id.to_string()),
            ("lang", locale),
            // No "extra" parameter = only basic info (id, name, image)
        ];

        let http_response = self
            .signed_get(
                &url,
                "artistget",
                &query
                    .iter()
                    .map(|(k, v)| (*k, v.clone()))
                    .collect::<Vec<_>>(),
            )
            .await?;
        log::debug!(
            "[API] get_artist_basic({}) status={}",
            artist_id,
            http_response.status()
        );
        let response: Value = http_response.json().await?;

        Ok(serde_json::from_value(response)?)
    }

    /// Get artist by ID
    pub async fn get_artist(&self, artist_id: u64, with_albums: bool) -> Result<Artist> {
        self.get_artist_with_pagination(artist_id, with_albums, None, None)
            .await
    }

    /// Get artist detail by ID with albums, playlists, and appears-on tracks
    pub async fn get_artist_detail(
        &self,
        artist_id: u64,
        limit: Option<u32>,
        offset: Option<u32>,
    ) -> Result<Artist> {
        let url = endpoints::build_url(paths::ARTIST_GET);
        let locale = self.locale().await;
        let mut query = vec![
            ("artist_id", artist_id.to_string()),
            ("extra", "albums,tracks_appears_on,playlists".to_string()),
            ("lang", locale),
        ];
        if let Some(l) = limit {
            query.push(("limit", l.to_string()));
        }
        if let Some(o) = offset {
            query.push(("offset", o.to_string()));
        }

        let http_response = self
            .signed_get(
                &url,
                "artistget",
                &query
                    .iter()
                    .map(|(k, v)| (*k, v.clone()))
                    .collect::<Vec<_>>(),
            )
            .await?;
        log::debug!(
            "[API] get_artist_detail({}) status={}",
            artist_id,
            http_response.status()
        );
        let response: Value = http_response.json().await?;

        Ok(serde_json::from_value(response)?)
    }

    /// Get artist by ID with album pagination
    pub async fn get_artist_with_pagination(
        &self,
        artist_id: u64,
        with_albums: bool,
        limit: Option<u32>,
        offset: Option<u32>,
    ) -> Result<Artist> {
        self.get_artist_with_pagination_and_locale(artist_id, with_albums, limit, offset, None)
            .await
    }

    /// Get artist by ID with album pagination and optional locale override
    /// Use locale_override to force a specific language (e.g., "en" for genre checking)
    pub async fn get_artist_with_pagination_and_locale(
        &self,
        artist_id: u64,
        with_albums: bool,
        limit: Option<u32>,
        offset: Option<u32>,
        locale_override: Option<&str>,
    ) -> Result<Artist> {
        let url = endpoints::build_url(paths::ARTIST_GET);
        let locale = match locale_override {
            Some(l) => l.to_string(),
            None => self.locale().await,
        };
        let mut query = vec![("artist_id", artist_id.to_string()), ("lang", locale)];
        if with_albums {
            query.push(("extra", "albums".to_string()));
        }
        if let Some(l) = limit {
            query.push(("limit", l.to_string()));
        }
        if let Some(o) = offset {
            query.push(("offset", o.to_string()));
        }

        let http_response = self
            .signed_get(
                &url,
                "artistget",
                &query
                    .iter()
                    .map(|(k, v)| (*k, v.clone()))
                    .collect::<Vec<_>>(),
            )
            .await?;
        log::debug!(
            "[API] get_artist({}, albums={}) status={}",
            artist_id,
            with_albums,
            http_response.status()
        );
        let response: Value = http_response.json().await?;

        Ok(serde_json::from_value(response)?)
    }

    /// Get playlist metadata + ordered track IDs (lightweight, no full Track objects).
    /// Uses `playlist/get?extra=track_ids` which returns the playlist with a flat
    /// array of track IDs instead of nested Track objects.
    pub async fn get_playlist_track_ids(&self, playlist_id: u64) -> Result<PlaylistWithTrackIds> {
        let url = endpoints::build_url(paths::PLAYLIST_GET);
        let http_response = self
            .signed_get(
                &url,
                "playlistget",
                &[
                    ("playlist_id", playlist_id.to_string()),
                    ("extra", "track_ids".to_string()),
                ],
            )
            .await?;
        log::debug!(
            "[API] get_playlist_track_ids({}) status={}",
            playlist_id,
            http_response.status()
        );
        let response: Value = http_response.json().await?;
        let result: PlaylistWithTrackIds = serde_json::from_value(response)?;
        log::info!(
            "[API] get_playlist_track_ids({}) — {} track IDs",
            playlist_id,
            result.track_ids.len()
        );
        Ok(result)
    }

    /// Fetch full Track objects for a batch of track IDs.
    /// Uses the `track/getList` endpoint, which caps at 50 IDs per call,
    /// so larger inputs are split into 50-ID windows and fetched serially.
    /// Input order is preserved in the returned vector.
    pub async fn get_tracks_batch(&self, track_ids: &[u64]) -> Result<Vec<Track>> {
        const MAX_PER_CALL: usize = 50;

        if track_ids.is_empty() {
            return Ok(Vec::new());
        }

        if track_ids.len() <= MAX_PER_CALL {
            return self.get_tracks_batch_chunk(track_ids).await;
        }

        log::debug!(
            "[API] get_tracks_batch chunking {} IDs into {}-windows",
            track_ids.len(),
            MAX_PER_CALL
        );
        let mut all = Vec::with_capacity(track_ids.len());
        for chunk in track_ids.chunks(MAX_PER_CALL) {
            let mut tracks = self.get_tracks_batch_chunk(chunk).await?;
            all.append(&mut tracks);
        }
        Ok(all)
    }

    /// Single `track/getList` POST. Caller is responsible for keeping
    /// `track_ids.len() <= 50` — `get_tracks_batch` handles that.
    async fn get_tracks_batch_chunk(&self, track_ids: &[u64]) -> Result<Vec<Track>> {
        let url = endpoints::build_url(paths::TRACK_GET_LIST);
        let headers = self.api_headers().await?;
        let timestamp = get_timestamp();
        let secret = self.secret().await?;
        let ids_str: String = track_ids
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let sig = sign_request(
            "trackgetList",
            &[("tracks_id", &ids_str)],
            timestamp,
            &secret,
        );

        let body = serde_json::json!({ "tracks_id": track_ids });
        log::debug!("[API] get_tracks_batch POST ({} IDs)", track_ids.len());

        let http_response = self
            .http()?
            .post(&url)
            .headers(headers)
            .query(&[("request_ts", timestamp.to_string()), ("request_sig", sig)])
            .json(&body)
            .send()
            .await?;

        let status = http_response.status();
        log::debug!("[API] get_tracks_batch POST status={}", status);

        let value: Value = http_response.json().await?;

        // Response: { "tracks": { "total": N, "items": [...] } }
        let items = value
            .get("tracks")
            .and_then(|t| t.get("items"))
            .ok_or_else(|| {
                let preview = serde_json::to_string(&value)
                    .unwrap_or_default()
                    .chars()
                    .take(500)
                    .collect::<String>();
                ApiError::ApiResponse(format!(
                    "Missing tracks.items in getList response: {}",
                    preview
                ))
            })?;

        let tracks: Vec<Track> = serde_json::from_value(items.clone())?;
        log::debug!("[API] get_tracks_batch returned {} tracks", tracks.len());
        Ok(tracks)
    }

    /// Get playlist by ID (paginates automatically to fetch all tracks)
    ///
    /// After the first page, remaining pages are fetched concurrently
    /// since we know the total track count from the first response.
    pub async fn get_playlist(&self, playlist_id: u64) -> Result<Playlist> {
        let url = endpoints::build_url(paths::PLAYLIST_GET);
        const PAGE_SIZE: u32 = 500;

        let start = std::time::Instant::now();

        // First page — gives us metadata + total track count
        let http_response = self
            .signed_get(
                &url,
                "playlistget",
                &[
                    ("playlist_id", playlist_id.to_string()),
                    ("limit", PAGE_SIZE.to_string()),
                    ("offset", "0".to_string()),
                    ("extra", "tracks".to_string()),
                ],
            )
            .await?;
        log::debug!(
            "[API] get_playlist({}) status={}",
            playlist_id,
            http_response.status()
        );
        let response: Value = http_response.json().await?;
        let mut playlist: Playlist = serde_json::from_value(response)?;

        // Fetch remaining pages concurrently
        if let Some(ref mut container) = playlist.tracks {
            let total = container.total;
            let fetched = container.items.len() as u32;

            if fetched < total {
                // Build all remaining page offsets
                let offsets: Vec<u32> = (fetched..total).step_by(PAGE_SIZE as usize).collect();
                log::debug!(
                    "[API] get_playlist({}) fetching {} remaining pages concurrently ({}/{})",
                    playlist_id,
                    offsets.len(),
                    fetched,
                    total
                );

                // Prepare headers and per-page signatures for concurrent requests
                let headers = self.api_headers().await?;
                let secret = self.secret().await.unwrap_or_default();

                // Offline gate checked ONCE for the whole page batch (the
                // first page above already passed through it) — not inside
                // the per-page loop.
                let gated_http = self.http()?;

                // Launch all page requests concurrently
                let futures: Vec<_> = offsets
                    .iter()
                    .map(|&offset| {
                        let http = gated_http;
                        let url = &url;
                        let headers = headers.clone();
                        let pid = playlist_id.to_string();
                        let limit = PAGE_SIZE.to_string();
                        let offset_str = offset.to_string();
                        let ts = get_timestamp();
                        let sig = sign_request(
                            "playlistget",
                            &[
                                ("extra", "tracks"),
                                ("limit", &limit),
                                ("offset", &offset_str),
                                ("playlist_id", &pid),
                            ],
                            ts,
                            &secret,
                        );
                        let ts_str = ts.to_string();
                        async move {
                            let resp = http
                                .get(url)
                                .headers(headers)
                                .query(&[
                                    ("playlist_id", pid.as_str()),
                                    ("limit", limit.as_str()),
                                    ("offset", offset_str.as_str()),
                                    ("extra", "tracks"),
                                    ("request_ts", ts_str.as_str()),
                                    ("request_sig", sig.as_str()),
                                ])
                                .send()
                                .await?;
                            let value: Value = resp.json().await?;
                            let page: Playlist = serde_json::from_value(value)?;
                            Ok::<_, anyhow::Error>((offset, page))
                        }
                    })
                    .collect();

                let results = futures_util::future::join_all(futures).await;

                // Collect results sorted by offset to maintain track order
                let mut pages: Vec<(u32, Playlist)> = Vec::new();
                for result in results {
                    match result {
                        Ok(page) => pages.push(page),
                        Err(e) => {
                            log::warn!(
                                "[API] get_playlist({}) page fetch failed: {}",
                                playlist_id,
                                e
                            );
                            // Continue with what we have
                        }
                    }
                }
                pages.sort_by_key(|(offset, _)| *offset);

                // Append tracks in order
                for (_, page_playlist) in pages {
                    if let Some(page_tracks) = page_playlist.tracks {
                        if !page_tracks.items.is_empty() {
                            container.items.extend(page_tracks.items);
                        }
                    }
                }
            }
        }

        let elapsed = start.elapsed();
        log::debug!(
            "[API] get_playlist({}) complete: {} tracks in {:.2}s",
            playlist_id,
            playlist.tracks.as_ref().map(|t| t.items.len()).unwrap_or(0),
            elapsed.as_secs_f64()
        );

        Ok(playlist)
    }

    /// Get label page (aggregated: top tracks, releases, playlists, artists)
    pub async fn get_label_page(&self, label_id: u64) -> Result<LabelPageData> {
        let url = endpoints::build_url(paths::LABEL_PAGE);

        log::debug!("[API] get_label_page({})", label_id);
        let response: serde_json::Value = self
            .signed_get(&url, "labelpage", &[("label_id", label_id.to_string())])
            .await?
            .json()
            .await?;

        Ok(serde_json::from_value(response)?)
    }

    /// Enumerate the award catalog (/award/explore). Paginated.
    /// Returns raw JSON items so callers can extract just (id, name).
    pub async fn get_award_explore(&self, limit: u32, offset: u32) -> Result<serde_json::Value> {
        let url = endpoints::build_url(paths::AWARD_EXPLORE);
        log::debug!(
            "[API] get_award_explore(limit={}, offset={})",
            limit,
            offset
        );
        let response: serde_json::Value = self
            .signed_get_auth(
                &url,
                "awardexplore",
                &[("limit", limit.to_string()), ("offset", offset.to_string())],
            )
            .await?
            .json()
            .await?;
        Ok(response)
    }

    /// Get award page — hero info + categorized award-winning releases.
    /// Mirrors Android's AwardDto shape from /award/page. Uses auth
    /// headers (app_id + user token) — region-gated catalog.
    pub async fn get_award_page(&self, award_id: &str) -> Result<qbz_models::AwardPageData> {
        let url = endpoints::build_url(paths::AWARD_PAGE);
        log::debug!("[API] get_award_page({})", award_id);
        let response: serde_json::Value = self
            .signed_get_auth(&url, "awardpage", &[("award_id", award_id.to_string())])
            .await?
            .json()
            .await?;
        Ok(serde_json::from_value(response)?)
    }

    /// Get paginated albums for a single award (/award/getAlbums).
    /// Response is V2AlbumGenericListDto = {has_more, items:[DiscographyAlbumDto]}
    /// per k20/b.java. DiscographyAlbumDto carries artists[] plural and
    /// artist singular as an optional DiscographyArtistDto — we backfill
    /// artist from artists[0] when missing (same trick as release watch).
    pub async fn get_award_albums(
        &self,
        award_id: &str,
        limit: u32,
        offset: u32,
    ) -> Result<SearchResultsPage<Album>> {
        let url = endpoints::build_url(paths::AWARD_GET_ALBUMS);
        log::debug!(
            "[API] get_award_albums({}, limit={}, offset={})",
            award_id,
            limit,
            offset
        );
        let http_response = self
            .signed_get_auth(
                &url,
                "awardgetAlbums",
                &[
                    ("award_id", award_id.to_string()),
                    ("limit", limit.to_string()),
                    ("offset", offset.to_string()),
                ],
            )
            .await?;
        let status = http_response.status();
        let body_text = http_response.text().await?;
        if !status.is_success() {
            log::warn!(
                "[API] get_award_albums non-success body (first 400 chars): {}",
                body_text.chars().take(400).collect::<String>()
            );
            return Err(ApiError::ApiResponse(format!(
                "get_award_albums status {}",
                status
            )));
        }
        let response: serde_json::Value = serde_json::from_str(&body_text)?;

        // Legacy shape some endpoints use: {albums: {items, total, offset, limit}}
        if let Some(albums_obj) = response.get("albums") {
            return Ok(serde_json::from_value(albums_obj.clone())?);
        }

        // Current shape: V2AlbumGenericListDto {has_more, items}.
        let mut items_value = response
            .get("items")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        if let Some(items_arr) = items_value.as_array_mut() {
            for item in items_arr {
                if let Some(obj) = item.as_object_mut() {
                    let needs_backfill = obj.get("artist").map(|v| v.is_null()).unwrap_or(true);
                    if needs_backfill {
                        if let Some(first_artist) = obj
                            .get("artists")
                            .and_then(|a| a.as_array())
                            .and_then(|arr| arr.first())
                            .cloned()
                        {
                            obj.insert("artist".to_string(), first_artist);
                        }
                    }
                }
            }
        }
        let items: Vec<Album> = serde_json::from_value(items_value).map_err(|e| {
            log::warn!(
                "[API] get_award_albums items parse error: {}. Body (first 600 chars): {}",
                e,
                body_text.chars().take(600).collect::<String>()
            );
            ApiError::ApiResponse(format!("get_award_albums items parse: {}", e))
        })?;

        let has_more = response
            .get("has_more")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let total_hint = if has_more {
            offset + items.len() as u32 + 1
        } else {
            offset + items.len() as u32
        };
        log::info!(
            "[API] get_award_albums({}) parsed {} items (has_more={})",
            award_id,
            items.len(),
            has_more
        );
        Ok(SearchResultsPage::<Album> {
            items,
            total: total_hint,
            offset,
            limit,
        })
    }

    /// Get label explore (discover more labels)
    pub async fn get_label_explore(&self, limit: u32, offset: u32) -> Result<LabelExploreResponse> {
        let url = endpoints::build_url(paths::LABEL_EXPLORE);

        log::debug!(
            "[API] get_label_explore(limit={}, offset={})",
            limit,
            offset
        );
        let response: serde_json::Value = self
            .signed_get(
                &url,
                "labelexplore",
                &[("limit", limit.to_string()), ("offset", offset.to_string())],
            )
            .await?
            .json()
            .await?;

        Ok(serde_json::from_value(response)?)
    }

    /// Get a label's album catalog (paginated, filterable).
    ///
    /// Replaces the legacy `/label/get?extra=albums` path.
    #[allow(clippy::too_many_arguments)]
    pub async fn get_label_albums(
        &self,
        label_id: u64,
        limit: u32,
        offset: u32,
        sort: Option<&str>,
        order: Option<&str>,
        genre_ids: Option<&str>,
        from_date: Option<&str>,
        to_date: Option<&str>,
    ) -> Result<LabelListPage<Album>> {
        let url = endpoints::build_url(paths::LABEL_GET_ALBUMS);
        let mut params: Vec<(&str, String)> = vec![
            ("label_id", label_id.to_string()),
            ("limit", limit.to_string()),
            ("offset", offset.to_string()),
        ];
        if let Some(v) = sort {
            params.push(("sort", v.to_string()));
        }
        if let Some(v) = order {
            params.push(("order", v.to_string()));
        }
        if let Some(v) = genre_ids {
            params.push(("genre_ids", v.to_string()));
        }
        if let Some(v) = from_date {
            params.push(("from_date", v.to_string()));
        }
        if let Some(v) = to_date {
            params.push(("to_date", v.to_string()));
        }

        log::debug!(
            "[API] get_label_albums({}, limit={}, offset={})",
            label_id,
            limit,
            offset
        );
        let response: Value = self
            .signed_get(&url, "labelgetalbums", &params)
            .await?
            .json()
            .await?;
        Ok(serde_json::from_value(response)?)
    }

    /// Get a label's upcoming releases.
    pub async fn get_label_next_releases(
        &self,
        label_id: u64,
        limit: u32,
        offset: u32,
        genre_ids: Option<&str>,
    ) -> Result<LabelListPage<Album>> {
        let url = endpoints::build_url(paths::LABEL_GET_NEXT_RELEASES);
        let mut params: Vec<(&str, String)> = vec![
            ("label_id", label_id.to_string()),
            ("limit", limit.to_string()),
            ("offset", offset.to_string()),
        ];
        if let Some(v) = genre_ids {
            params.push(("genre_ids", v.to_string()));
        }

        log::debug!("[API] get_label_next_releases({})", label_id);
        let response: Value = self
            .signed_get(&url, "labelgetnextreleases", &params)
            .await?
            .json()
            .await?;
        Ok(serde_json::from_value(response)?)
    }

    /// Get a label's press-awarded releases.
    pub async fn get_label_awarded_releases(
        &self,
        label_id: u64,
        limit: u32,
        offset: u32,
        sort: Option<&str>,
        order: Option<&str>,
        genre_ids: Option<&str>,
    ) -> Result<LabelListPage<Album>> {
        let url = endpoints::build_url(paths::LABEL_GET_AWARDED_RELEASES);
        let mut params: Vec<(&str, String)> = vec![
            ("label_id", label_id.to_string()),
            ("limit", limit.to_string()),
            ("offset", offset.to_string()),
        ];
        if let Some(v) = sort {
            params.push(("sort", v.to_string()));
        }
        if let Some(v) = order {
            params.push(("order", v.to_string()));
        }
        if let Some(v) = genre_ids {
            params.push(("genre_ids", v.to_string()));
        }

        log::debug!("[API] get_label_awarded_releases({})", label_id);
        let response: Value = self
            .signed_get(&url, "labelgetawardedreleases", &params)
            .await?
            .json()
            .await?;
        Ok(serde_json::from_value(response)?)
    }

    /// Get a label's curated playlists (paginated).
    pub async fn get_label_playlists(
        &self,
        label_id: u64,
        limit: u32,
        offset: u32,
    ) -> Result<LabelListPage<Playlist>> {
        let url = endpoints::build_url(paths::LABEL_GET_PLAYLISTS);
        log::debug!("[API] get_label_playlists({})", label_id);
        let response: Value = self
            .signed_get(
                &url,
                "labelgetplaylists",
                &[
                    ("label_id", label_id.to_string()),
                    ("limit", limit.to_string()),
                    ("offset", offset.to_string()),
                ],
            )
            .await?
            .json()
            .await?;
        Ok(serde_json::from_value(response)?)
    }

    /// Get a label's top artists (paginated).
    pub async fn get_label_top_artists(
        &self,
        label_id: u64,
        limit: u32,
        offset: u32,
    ) -> Result<LabelListPage<Artist>> {
        let url = endpoints::build_url(paths::LABEL_GET_TOP_ARTISTS);
        log::debug!("[API] get_label_top_artists({})", label_id);
        let response: Value = self
            .signed_get(
                &url,
                "labelgettopartists",
                &[
                    ("label_id", label_id.to_string()),
                    ("limit", limit.to_string()),
                    ("offset", offset.to_string()),
                ],
            )
            .await?
            .json()
            .await?;
        Ok(serde_json::from_value(response)?)
    }

    /// Get a label's editorial / story content.
    pub async fn get_label_story(
        &self,
        label_id: u64,
        limit: u32,
        offset: u32,
    ) -> Result<LabelStoryResponse> {
        let url = endpoints::build_url(paths::LABEL_STORY);
        log::debug!("[API] get_label_story({})", label_id);
        let response: Value = self
            .signed_get(
                &url,
                "labelstory",
                &[
                    ("label_id", label_id.to_string()),
                    ("limit", limit.to_string()),
                    ("offset", offset.to_string()),
                ],
            )
            .await?
            .json()
            .await?;
        Ok(serde_json::from_value(response)?)
    }

    /// Bulk lookup a set of labels by ID (POST).
    ///
    /// Follows the same signing convention as `get_tracks_batch` (see
    /// `track/getList`): the sig covers the joined ID list as a query
    /// string key, and the JSON body carries the list itself.
    pub async fn get_label_list(&self, label_ids: &[u64]) -> Result<LabelGetListResponse> {
        let url = endpoints::build_url(paths::LABEL_GET_LIST);
        let headers = self.api_headers().await?;
        let timestamp = get_timestamp();
        let secret = self.secret().await?;
        let ids_str: String = label_ids
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let sig = sign_request(
            "labelgetList",
            &[("label_ids", &ids_str)],
            timestamp,
            &secret,
        );

        let body = serde_json::json!({ "label_ids": label_ids });
        log::debug!("[API] get_label_list POST ({} ids)", label_ids.len());

        let response: Value = self
            .http()?
            .post(&url)
            .headers(headers)
            .query(&[("request_ts", timestamp.to_string()), ("request_sig", sig)])
            .json(&body)
            .send()
            .await?
            .json()
            .await?;
        Ok(serde_json::from_value(response)?)
    }

    // === Authenticated endpoints ===

    /// Get stream URL for a track (requires auth + signature)
    /// Raw `/track/getFileUrl` (`intent=stream`): HTTP status and body,
    /// verbatim, no parsing. Probe support only — the typed reader is
    /// [`Self::get_stream_url`]. Same signing, same headers, same breaker.
    pub async fn get_stream_url_raw(
        &self,
        track_id: u64,
        quality: Quality,
    ) -> Result<(u16, String)> {
        self.forbidden_guard()?;
        let url = endpoints::build_url(paths::TRACK_GET_FILE_URL);
        let timestamp = get_timestamp();
        let secret = self.secret().await?;
        let signature = sign_get_file_url(track_id, quality.id(), timestamp, &secret);
        let response = self
            .http()?
            .get(&url)
            .headers(self.authenticated_headers().await?)
            .query(&[
                ("track_id", track_id.to_string()),
                ("format_id", quality.id().to_string()),
                ("intent", "stream".to_string()),
                ("request_ts", timestamp.to_string()),
                ("request_sig", signature),
            ])
            .send()
            .await?;
        let status = response.status();
        self.note_forbidden_status(status);
        let body = response.text().await?;
        Ok((status.as_u16(), body))
    }

    pub async fn get_stream_url(&self, track_id: u64, quality: Quality) -> Result<StreamUrl> {
        // Back off before the network if the 403 breaker is open (issue #637).
        self.forbidden_guard()?;
        log::info!(
            "Getting stream URL for track {} with quality {:?}",
            track_id,
            quality
        );
        let url = endpoints::build_url(paths::TRACK_GET_FILE_URL);
        let timestamp = get_timestamp();
        log::debug!("Getting secret for signing...");
        let secret = self.secret().await?;
        log::debug!("Secret obtained, signing request...");
        let signature = sign_get_file_url(track_id, quality.id(), timestamp, &secret);

        log::debug!("Sending stream URL request...");
        let response = self
            .http()?
            .get(&url)
            .headers(self.authenticated_headers().await?)
            .query(&[
                ("track_id", track_id.to_string()),
                ("format_id", quality.id().to_string()),
                ("intent", "stream".to_string()),
                ("request_ts", timestamp.to_string()),
                ("request_sig", signature),
            ])
            .send()
            .await?;

        let status = response.status();
        log::info!("Stream URL response status: {}", status);
        // Feed the breaker: a 403 counts toward opening it; a 200 resets it.
        self.note_forbidden_status(status);
        match status {
            StatusCode::OK => {
                let json: Value = response.json().await?;
                log::debug!(
                    "Stream URL response JSON keys: {:?}",
                    json.as_object().map(|o| o.keys().collect::<Vec<_>>())
                );

                // Check for restrictions
                let restrictions: Vec<StreamRestriction> = json
                    .get("restrictions")
                    .and_then(|v| serde_json::from_value(v.clone()).ok())
                    .unwrap_or_default();

                // Validate that we got an actual URL (track may be unavailable)
                let url = json["url"].as_str().unwrap_or("").to_string();
                if url.is_empty() {
                    // Log the restriction codes for debugging
                    let restriction_codes: Vec<&str> =
                        restrictions.iter().map(|r| r.code.as_str()).collect();
                    log::warn!(
                        "Stream URL missing for track {} - restrictions: {:?}",
                        track_id,
                        restriction_codes
                    );
                    return Err(ApiError::TrackUnavailable(track_id));
                }

                Ok(StreamUrl {
                    url,
                    format_id: json["format_id"].as_u64().unwrap_or(0) as u32,
                    mime_type: json["mime_type"].as_str().unwrap_or("").to_string(),
                    sampling_rate: json["sampling_rate"].as_f64().unwrap_or(0.0),
                    bit_depth: json["bit_depth"].as_u64().map(|v| v as u32),
                    track_id,
                    restrictions,
                    sample: json["sample"].as_bool().unwrap_or(false),
                })
            }
            StatusCode::BAD_REQUEST => Err(ApiError::InvalidAppSecret),
            StatusCode::UNAUTHORIZED => Err(ApiError::AuthenticationError(
                "stream URL 401 — user auth token invalid or expired".to_string(),
            )),
            StatusCode::FORBIDDEN => {
                let preview = body_preview(response).await;
                log::warn!("Stream URL 403 for track {}{}", track_id, preview);
                Err(ApiError::Forbidden(preview))
            }
            status => Err(ApiError::ApiResponse(format!(
                "Unexpected status: {}",
                status
            ))),
        }
    }

    /// Get stream URL with quality fallback
    pub async fn get_stream_url_with_fallback(
        &self,
        track_id: u64,
        preferred: Quality,
    ) -> Result<StreamUrl> {
        self.get_stream_url_with_acquisition(track_id, preferred)
            .await
            .map(|(url, _)| url)
    }

    /// Also return the quality of the successful request. The response's
    /// format_id describes the available file, not whether fallback occurred.
    pub async fn get_stream_url_with_acquisition(
        &self,
        track_id: u64,
        preferred: Quality,
    ) -> Result<(StreamUrl, Quality)> {
        Self::resolve_stream_url_with_fallback(track_id, preferred, |quality| {
            self.get_stream_url(track_id, quality)
        }).await
    }

    async fn resolve_stream_url_with_fallback<F, Fut>(
        track_id: u64,
        preferred: Quality,
        mut fetch: F,
    ) -> Result<(StreamUrl, Quality)>
    where
        F: FnMut(Quality) -> Fut,
        Fut: std::future::Future<Output = Result<StreamUrl>>,
    {
        log::info!(
            "Getting stream URL with fallback for track {}, preferred quality: {:?}",
            track_id,
            preferred
        );
        let qualities = Quality::fallback_order();
        let start_idx = qualities.iter().position(|q| *q == preferred).unwrap_or(0);

        let mut track_unavailable = false;

        for quality in &qualities[start_idx..] {
            log::info!("Trying quality: {:?}", quality);
            match fetch(*quality).await {
                Ok(url) if !url.has_restrictions() => {
                    log::info!(
                        "Got stream URL for requested quality format_id={}",
                        quality.id()
                    );
                    return Ok((url, *quality));
                }
                Ok(_) => {
                    log::info!("Quality {:?} has restrictions, trying next", quality);
                    continue;
                }
                Err(ApiError::InvalidAppSecret) => {
                    log::error!("Invalid app secret");
                    return Err(ApiError::InvalidAppSecret);
                }
                // A 403 (or an open breaker, or a 401) is NOT a per-quality
                // restriction — every quality would 403 the same way. Abort the
                // whole fallback loop immediately instead of firing 5 more
                // requests per track and feeding the storm (issue #637).
                Err(
                    e @ (ApiError::Forbidden(_)
                    | ApiError::ForbiddenCircuitOpen(_)
                    | ApiError::AuthenticationError(_)),
                ) => {
                    log::warn!("Stream URL aborting quality fallback: {}", e);
                    return Err(e);
                }
                Err(ApiError::TrackUnavailable(_)) => {
                    // Track is completely unavailable on Qobuz
                    track_unavailable = true;
                    continue;
                }
                Err(e) => {
                    log::warn!("Quality {:?} failed: {}, trying next", quality, e);
                    continue;
                }
            }
        }

        // If all quality levels reported track unavailable, return that specific error
        if track_unavailable {
            log::error!("Track {} is no longer available on Qobuz", track_id);
            return Err(ApiError::TrackUnavailable(track_id));
        }

        log::error!("No quality available for track {}", track_id);
        Err(ApiError::NoQualityAvailable)
    }

    /// Get user favorites (requires auth + signature)
    pub async fn get_favorites(&self, fav_type: &str, limit: u32, offset: u32) -> Result<Value> {
        // Back off before the network if the 403 breaker is open (issue #637).
        self.forbidden_guard()?;
        let url = endpoints::build_url(paths::FAVORITE_GET_USER_FAVORITES);
        let timestamp = get_timestamp();
        let secret = self.secret().await?;
        let signature = sign_get_favorites(timestamp, &secret);

        let http_response = self
            .http()?
            .get(&url)
            .headers(self.authenticated_headers().await?)
            .query(&[
                ("type", fav_type),
                ("limit", &limit.to_string()),
                ("offset", &offset.to_string()),
                ("request_ts", &timestamp.to_string()),
                ("request_sig", &signature),
            ])
            .send()
            .await?;
        let status = http_response.status();
        log::debug!("[API] get_favorites({}) status={}", fav_type, status);
        // Feed the breaker AND check status BEFORE decoding: a 403's body is not
        // our JSON envelope (it's an edge/WAF HTML/empty body), so a bare
        // `.json()` surfaced it as a misleading "error decoding response body"
        // instead of the real 403 (issue #637).
        self.note_forbidden_status(status);
        if !status.is_success() {
            let preview = body_preview(http_response).await;
            if status == StatusCode::FORBIDDEN {
                log::warn!("get_favorites({}) 403{}", fav_type, preview);
                return Err(ApiError::Forbidden(preview));
            }
            return Err(ApiError::ApiResponse(format!(
                "get_favorites failed with status {}{}",
                status, preview
            )));
        }
        let response: Value = http_response.json().await?;

        Ok(response)
    }

    /// Get user's playlists
    pub async fn get_user_playlists(&self) -> Result<Vec<Playlist>> {
        let url = endpoints::build_url(paths::PLAYLIST_GET_USER_PLAYLISTS);
        let http_response = self
            .signed_get_auth(&url, "playlistgetUserPlaylists", &[])
            .await?;
        log::debug!("[API] get_user_playlists status={}", http_response.status());
        let response: Value = http_response.json().await?;

        let playlists = response
            .get("playlists")
            .and_then(|p| p.get("items"))
            .ok_or_else(|| ApiError::ApiResponse("No playlists in response".to_string()))?;

        Ok(serde_json::from_value(playlists.clone())?)
    }

    /// Search playlists
    pub async fn search_playlists(
        &self,
        query: &str,
        limit: u32,
        offset: u32,
    ) -> Result<SearchResultsPage<Playlist>> {
        let url = endpoints::build_url(paths::PLAYLIST_SEARCH);
        let timestamp = get_timestamp();
        let secret = self.secret().await?;
        let signature = sign_search(
            "playlistsearch",
            query,
            limit,
            offset,
            None,
            timestamp,
            &secret,
        );
        let limit_str = limit.to_string();
        let offset_str = offset.to_string();
        let ts_str = timestamp.to_string();

        let http_response = self
            .http()?
            .get(&url)
            .headers(self.api_headers().await?)
            .query(&[
                ("query", query),
                ("limit", &limit_str),
                ("offset", &offset_str),
                ("request_ts", &ts_str),
                ("request_sig", &signature),
            ])
            .send()
            .await?;
        log::debug!("[API] search_playlists status={}", http_response.status());
        let response: Value = http_response.json().await?;

        let playlists = response
            .get("playlists")
            .ok_or_else(|| ApiError::ApiResponse("No playlists in response".to_string()))?;

        Ok(serde_json::from_value(playlists.clone())?)
    }

    /// Create a new playlist
    pub async fn create_playlist(
        &self,
        name: &str,
        description: Option<&str>,
        is_public: bool,
    ) -> Result<Playlist> {
        let url = endpoints::build_url(paths::PLAYLIST_CREATE);

        let mut params = vec![
            ("name", name.to_string()),
            ("is_public", is_public.to_string()),
        ];
        if let Some(desc) = description {
            params.push(("description", desc.to_string()));
        }

        let params: Vec<(&str, String)> = params.iter().map(|(k, v)| (*k, v.clone())).collect();
        let response: Playlist = self
            .signed_get_auth(&url, "playlistcreate", &params)
            .await?
            .json()
            .await?;

        Ok(response)
    }

    /// Delete a playlist
    pub async fn delete_playlist(&self, playlist_id: u64) -> Result<()> {
        let url = endpoints::build_url(paths::PLAYLIST_DELETE);

        self.signed_get_auth(
            &url,
            "playlistdelete",
            &[("playlist_id", playlist_id.to_string())],
        )
        .await?;

        Ok(())
    }

    /// Add tracks to a playlist
    pub async fn add_tracks_to_playlist(&self, playlist_id: u64, track_ids: &[u64]) -> Result<()> {
        let url = endpoints::build_url(paths::PLAYLIST_ADD_TRACKS);
        let track_ids_str = track_ids
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(",");

        self.signed_get_auth(
            &url,
            "playlistaddTracks",
            &[
                ("playlist_id", playlist_id.to_string()),
                ("track_ids", track_ids_str),
            ],
        )
        .await?;

        Ok(())
    }

    /// Remove tracks from a playlist
    pub async fn remove_tracks_from_playlist(
        &self,
        playlist_id: u64,
        playlist_track_ids: &[u64],
    ) -> Result<()> {
        let url = endpoints::build_url(paths::PLAYLIST_DELETE_TRACKS);
        let track_ids_str = playlist_track_ids
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(",");

        self.signed_get_auth(
            &url,
            "playlistdeleteTracks",
            &[
                ("playlist_id", playlist_id.to_string()),
                ("playlist_track_ids", track_ids_str),
            ],
        )
        .await?;

        Ok(())
    }

    /// Move tracks WITHIN a playlist — `/playlist/updateTracksPosition`.
    ///
    /// `playlist_track_ids` are MEMBERSHIP ids, the same id space
    /// [`Self::remove_tracks_from_playlist`] takes — not catalog track ids.
    /// `insert_before` is the 0-based slot the moved rows must land in front
    /// of, counted against the playlist AS IT IS at call time.
    ///
    /// # Provenance, because this one is not in the current spec
    ///
    /// The argument shape is the DESKTOP web-player capture's
    /// (`qbz-nix-docs/qobuz-api-delta-desktop-8.2.0-b015.md:95` —
    /// `{playlist_id, playlist_track_ids, insert_before}`). The decompiled
    /// Android route table lists the same path, and that entry is corroboration
    /// and nothing more: CLAUDE.md forbids inferring endpoints from it.
    ///
    /// GET + [`Self::signed_get_auth`], exactly like the two neighbours this
    /// call is sequenced with. The capture records a POST, but every playlist
    /// write in this client is a signed GET against api.json/0.2 — including
    /// `addTracks` and `deleteTracks`, whose captures were POSTs too — and the
    /// signature method name follows the same `<namespace><Method>` rule the
    /// neighbours use, hence `playlistupdateTracksPosition`.
    ///
    /// The exact reading of `insert_before` is the ONE part of this verb no
    /// capture in the repo pins down, so the request is logged in full at debug
    /// level and callers must treat a successful-but-wrong move as cosmetic —
    /// never as a reason to abort a repair that already landed.
    pub async fn update_playlist_tracks_position(
        &self,
        playlist_id: u64,
        playlist_track_ids: &[u64],
        insert_before: u32,
    ) -> Result<()> {
        let url = endpoints::build_url(paths::PLAYLIST_UPDATE_TRACKS_POSITION);
        let track_ids_str = playlist_track_ids
            .iter()
            .map(|id| id.to_string())
            .collect::<Vec<_>>()
            .join(",");
        log::debug!(
            "[API] updateTracksPosition playlist_id={} playlist_track_ids={} insert_before={}",
            playlist_id,
            track_ids_str,
            insert_before
        );

        self.signed_get_auth(
            &url,
            "playlistupdateTracksPosition",
            &[
                ("playlist_id", playlist_id.to_string()),
                ("playlist_track_ids", track_ids_str),
                ("insert_before", insert_before.to_string()),
            ],
        )
        .await?;

        Ok(())
    }

    /// Update playlist metadata
    pub async fn update_playlist(
        &self,
        playlist_id: u64,
        name: Option<&str>,
        description: Option<&str>,
        is_public: Option<bool>,
    ) -> Result<Playlist> {
        let url = endpoints::build_url(paths::PLAYLIST_UPDATE);

        let mut params = vec![("playlist_id", playlist_id.to_string())];
        if let Some(n) = name {
            params.push(("name", n.to_string()));
        }
        if let Some(d) = description {
            params.push(("description", d.to_string()));
        }
        if let Some(p) = is_public {
            params.push(("is_public", p.to_string()));
        }

        let params_ref: Vec<(&str, String)> = params.iter().map(|(k, v)| (*k, v.clone())).collect();
        let response: Playlist = self
            .signed_get_auth(&url, "playlistupdate", &params_ref)
            .await?
            .json()
            .await?;

        Ok(response)
    }

    /// Subscribe to a Qobuz playlist (follow it in the user's library)
    pub async fn subscribe_playlist(&self, playlist_id: u64) -> Result<()> {
        let url = endpoints::build_url(paths::PLAYLIST_SUBSCRIBE);

        let response = self
            .signed_get_auth(
                &url,
                "playlistsubscribe",
                &[("playlist_id", playlist_id.to_string())],
            )
            .await?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ApiError::ApiResponse(format!(
                "playlist/subscribe failed ({}): {}",
                status, body
            )));
        }

        Ok(())
    }

    /// Unsubscribe from a Qobuz playlist
    pub async fn unsubscribe_playlist(&self, playlist_id: u64) -> Result<()> {
        let url = endpoints::build_url(paths::PLAYLIST_UNSUBSCRIBE);

        let response = self
            .signed_get_auth(
                &url,
                "playlistunsubscribe",
                &[("playlist_id", playlist_id.to_string())],
            )
            .await?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(ApiError::ApiResponse(format!(
                "playlist/unsubscribe failed ({}): {}",
                status, body
            )));
        }

        Ok(())
    }

    /// Add item to favorites
    pub async fn add_favorite(&self, fav_type: &str, item_id: &str) -> Result<()> {
        let url = endpoints::build_url(paths::FAVORITE_CREATE);
        let type_key = format!("{}_ids", fav_type); // album_ids, track_ids, artist_ids

        let response = self
            .signed_get_auth(
                &url,
                "favoritecreate",
                &[(type_key.as_str(), item_id.to_string())],
            )
            .await?;

        if response.status().is_success() {
            Ok(())
        } else {
            Err(ApiError::ApiResponse(format!(
                "Failed to add favorite: {}",
                response.status()
            )))
        }
    }

    /// Remove item from favorites
    pub async fn remove_favorite(&self, fav_type: &str, item_id: &str) -> Result<()> {
        let url = endpoints::build_url(paths::FAVORITE_DELETE);
        let type_key = format!("{}_ids", fav_type);

        let response = self
            .signed_get_auth(
                &url,
                "favoritedelete",
                &[(type_key.as_str(), item_id.to_string())],
            )
            .await?;

        if response.status().is_success() {
            Ok(())
        } else {
            Err(ApiError::ApiResponse(format!(
                "Failed to remove favorite: {}",
                response.status()
            )))
        }
    }

    // ============ Artist Page Endpoints ============

    /// Get artist page (aggregated: bio, top tracks, releases, similar, playlists)
    pub async fn get_artist_page(
        &self,
        artist_id: u64,
        sort: Option<&str>,
    ) -> Result<PageArtistResponse> {
        let url = endpoints::build_url(paths::ARTIST_PAGE);
        let mut query = vec![("artist_id", artist_id.to_string())];
        if let Some(s) = sort {
            query.push(("sort", s.to_string()));
        }

        log::debug!("[API] get_artist_page({}) sort={:?}", artist_id, sort);
        let response: serde_json::Value = self
            .signed_get(
                &url,
                "artistpage",
                &query
                    .iter()
                    .map(|(k, v)| (*k, v.clone()))
                    .collect::<Vec<_>>(),
            )
            .await?
            .json()
            .await?;

        Ok(serde_json::from_value(response)?)
    }

    /// Get artist releases grid (paginated by release_type)
    pub async fn get_releases_grid(
        &self,
        artist_id: u64,
        release_type: &str,
        limit: u32,
        offset: u32,
        sort: Option<&str>,
    ) -> Result<ReleasesGridResponse> {
        let url = endpoints::build_url(paths::ARTIST_RELEASES_GRID);
        let mut query = vec![
            ("artist_id", artist_id.to_string()),
            ("release_type", release_type.to_string()),
            ("limit", limit.to_string()),
            ("offset", offset.to_string()),
        ];
        if let Some(s) = sort {
            query.push(("sort", s.to_string()));
        }

        log::debug!(
            "[API] get_releases_grid({}) type={} limit={} offset={}",
            artist_id,
            release_type,
            limit,
            offset
        );
        let response: serde_json::Value = self
            .signed_get(
                &url,
                "artistgetReleasesGrid",
                &query
                    .iter()
                    .map(|(k, v)| (*k, v.clone()))
                    .collect::<Vec<_>>(),
            )
            .await?
            .json()
            .await?;

        Ok(serde_json::from_value(response)?)
    }

    /// Get artist Magazine stories (editorial articles about the artist).
    /// REST header-auth only (no per-op signing). Web client calls offset=0 limit=2.
    pub async fn get_artist_story(
        &self,
        artist_id: u64,
        offset: u32,
        limit: u32,
    ) -> Result<ArtistStoryResponse> {
        let url = endpoints::build_url(paths::ARTIST_STORY);
        let query = vec![
            ("artist_id", artist_id.to_string()),
            ("offset", offset.to_string()),
            ("limit", limit.to_string()),
        ];

        log::debug!(
            "[API] get_artist_story({}) offset={} limit={}",
            artist_id,
            offset,
            limit
        );
        let response: serde_json::Value = self
            .signed_get(
                &url,
                "artiststory",
                &query
                    .iter()
                    .map(|(k, v)| (*k, v.clone()))
                    .collect::<Vec<_>>(),
            )
            .await?
            .json()
            .await?;

        Ok(serde_json::from_value(response)?)
    }

    // === CMAF streaming endpoints ===

    /// Ensure we have a valid CMAF session, renewing if expired.
    /// Returns `(session_id, infos)` for use with file/url and key derivation.
    ///
    /// Concurrency note: this method serializes concurrent session
    /// renewals. Without that, two overlapping callers could both see
    /// "no session" on the read side, each POST /session/start, each get
    /// DIFFERENT `infos`, and the second one to finish would overwrite
    /// the first in the cache. Any `get_file_url` response whose wrapped
    /// key was tied to the first session then unwrapped with the second
    /// session's key and blew up with AES-CBC "Unpad Error" — which
    /// manifested as prefetch CMAF failures + downloaded-but-gappy
    /// transitions between offline tracks.
    ///
    /// Fix: a double-checked lock pattern on the write guard. Fast path
    /// uses a read guard; slow path acquires the write guard, re-checks
    /// under exclusive ownership, and only one caller hits the network.
    pub async fn ensure_cmaf_session(&self) -> Result<(String, String)> {
        let now = get_timestamp();

        // Fast path: existing session with > 60s left.
        {
            let guard = self.cmaf_session.read().await;
            if let Some(ref cs) = *guard {
                if cs.expires_at > now + 60 {
                    return Ok((cs.session_id.clone(), cs.infos.clone()));
                }
            }
        }

        // Slow path: take the write lock and re-check. Concurrent callers
        // end up here one at a time; after the first finishes POST
        // session/start, the rest find the freshly-populated cache and
        // return without hitting the network.
        let mut guard = self.cmaf_session.write().await;
        if let Some(ref cs) = *guard {
            if cs.expires_at > now + 60 {
                return Ok((cs.session_id.clone(), cs.infos.clone()));
            }
        }

        // We're the one task that actually starts a session. Back off before
        // the network if the 403 breaker is open (issue #637) — a cached
        // session above is still served; only the network POST is gated.
        self.forbidden_guard()?;
        log::info!("[CMAF] Starting new session");
        let timestamp = get_timestamp();
        let sig = sign_session_start(timestamp);

        let url = endpoints::build_url(paths::SESSION_START);
        let response = self
            .http()?
            .post(&url)
            .headers(self.authenticated_headers().await?)
            .form(&[
                ("profile", "qbz-1"),
                ("request_ts", &timestamp.to_string()),
                ("request_sig", &sig),
            ])
            .send()
            .await?;

        let status = response.status();
        // Feed the breaker: a 403 here counts toward opening it; success resets.
        self.note_forbidden_status(status);
        if !status.is_success() {
            if status == StatusCode::FORBIDDEN {
                let preview = body_preview(response).await;
                log::warn!("[CMAF] session/start 403{}", preview);
                return Err(ApiError::Forbidden(preview));
            }
            return Err(ApiError::ApiResponse(format!(
                "session/start failed with status {}",
                status
            )));
        }

        let resp: SessionStartResponse = response.json().await?;
        let infos = resp.infos.unwrap_or_default();
        log::info!(
            "[CMAF] Session started: id={}..., expires_at={}",
            &resp.session_id[..resp.session_id.len().min(8)],
            resp.expires_at
        );

        let session_id = resp.session_id.clone();
        let infos_clone = infos.clone();

        *guard = Some(CmafSession {
            session_id: resp.session_id,
            infos,
            expires_at: resp.expires_at,
        });

        Ok((session_id, infos_clone))
    }

    /// Get CMAF segmented file URL for a track.
    ///
    /// This is the new streaming endpoint that returns encrypted CMAF segments
    /// instead of a direct file URL.
    pub async fn get_file_url(&self, track_id: u64, quality: Quality) -> Result<TrackFileUrl> {
        let format_id = quality.id();
        let url = endpoints::build_url(paths::FILE_URL);

        // Back off before the network if the 403 breaker is open (issue #637).
        self.forbidden_guard()?;

        // Retry transient failures (5xx / 429 / network blips) with backoff so
        // a momentary hiccup on the next track's file/url is not turned into a
        // queue skip. A real 404 → TrackUnavailable is terminal and returns
        // immediately to the (bounded) skip path. Issue #467.
        crate::retry::retry_transient(
            crate::retry::DEFAULT_MAX_ATTEMPTS,
            "CMAF file/url",
            ApiError::is_transient,
            |_attempt| {
                let url = url.clone();
                async move {
                    let (session_id, _infos) = self.ensure_cmaf_session().await?;

                    // Fresh timestamp + signature per attempt — the request
                    // signature is time-bound and would expire across retries.
                    let timestamp = get_timestamp();
                    let sig = sign_file_url(track_id, format_id, timestamp);

                    let mut headers = self.authenticated_headers().await?;
                    headers.insert(
                        "X-Session-Id",
                        reqwest::header::HeaderValue::from_str(&session_id).map_err(|_| {
                            ApiError::ApiResponse("Invalid session ID format".into())
                        })?,
                    );

                    let response = self
                        .http()?
                        .get(&url)
                        .headers(headers)
                        .query(&[
                            ("track_id", track_id.to_string()),
                            ("format_id", format_id.to_string()),
                            ("intent", "stream".to_string()),
                            ("request_ts", timestamp.to_string()),
                            ("request_sig", sig),
                        ])
                        .send()
                        .await?;

                    let status = response.status();
                    log::info!(
                        "[CMAF] file/url track_id={} format_id={} status={}",
                        track_id,
                        format_id,
                        status
                    );
                    // Feed the breaker: 403 counts toward opening it; 2xx resets.
                    self.note_forbidden_status(status);

                    if !status.is_success() {
                        let code = status.as_u16();
                        if status == reqwest::StatusCode::FORBIDDEN {
                            let preview = body_preview(response).await;
                            log::warn!("[CMAF] file/url 403 for track {}{}", track_id, preview);
                            return Err(ApiError::Forbidden(preview));
                        }
                        return Err(if code == 404 {
                            ApiError::TrackUnavailable(track_id)
                        } else if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                            // Honor Retry-After (seconds) when the server sends it.
                            let retry_after = response
                                .headers()
                                .get(reqwest::header::RETRY_AFTER)
                                .and_then(|v| v.to_str().ok())
                                .and_then(|s| s.trim().parse::<u64>().ok())
                                .unwrap_or(2);
                            ApiError::RateLimited(retry_after)
                        } else if status.is_server_error() {
                            ApiError::ServerError(code)
                        } else {
                            ApiError::ApiResponse(format!("file/url failed with status {}", status))
                        });
                    }

                    let file_url: TrackFileUrl = response.json().await?;
                    log::info!(
                        "[CMAF] file/url result: segments={}, mime={:?}, sampling_rate={:?}",
                        file_url.n_segments,
                        file_url.mime_type,
                        file_url.sampling_rate
                    );

                    Ok(file_url)
                }
            },
        )
        .await
    }
}

impl Default for QobuzClient {
    fn default() -> Self {
        Self::new().expect("Failed to create client")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn available_master(format_id: u32) -> StreamUrl {
        StreamUrl {
            url: "https://cdn.example.test/audio".into(), format_id,
            mime_type: "audio/flac".into(),
            sampling_rate: if format_id == 6 { 44.1 } else { 96.0 },
            bit_depth: Some(if format_id == 6 { 16 } else { 24 }),
            track_id: 7, restrictions: Vec::new(), sample: false,
        }
    }

    #[tokio::test]
    async fn successful_request_keeps_available_format_without_extra_attempts() {
        for format_id in [6, 7] {
            let mut attempts = Vec::new();
            let (url, actual) = QobuzClient::resolve_stream_url_with_fallback(
                7, Quality::UltraHiRes, |quality| {
                    attempts.push(quality);
                    std::future::ready(Ok(available_master(format_id)))
                },
            ).await.unwrap();
            assert_eq!(attempts, vec![Quality::UltraHiRes]);
            assert_eq!(actual, Quality::UltraHiRes);
            assert_eq!(url.format_id, format_id);
        }
    }

    #[tokio::test]
    async fn successful_request_reports_actual_fallback_not_initial_preference() {
        let mut attempts = Vec::new();
        let (url, actual) = QobuzClient::resolve_stream_url_with_fallback(
            7, Quality::UltraHiRes, |quality| {
                attempts.push(quality);
                let mut url = available_master(7);
                if quality == Quality::UltraHiRes {
                    url.restrictions.push(qbz_models::StreamRestriction {
                        code: "FormatRestrictedByFormatAvailability".into(),
                    });
                }
                std::future::ready(Ok(url))
            },
        ).await.unwrap();
        assert_eq!(attempts, vec![Quality::UltraHiRes, Quality::HiRes]);
        assert_eq!(actual, Quality::HiRes);
        assert_eq!(url.format_id, 7);
    }

    #[tokio::test]
    async fn successful_request_aborts_auth_failure_without_lower_quality_requests() {
        for error in [ApiError::Forbidden("fixture".into()),
            ApiError::ForbiddenCircuitOpen(30),
            ApiError::AuthenticationError("fixture".into()), ApiError::InvalidAppSecret] {
            let expected = std::mem::discriminant(&error);
            let mut error = Some(error);
            let mut attempts = Vec::new();
            let result = QobuzClient::resolve_stream_url_with_fallback(
                7, Quality::UltraHiRes, |quality| {
                    attempts.push(quality);
                    std::future::ready(Err(error.take().expect("must abort after first failure")))
                },
            ).await;
            assert_eq!(std::mem::discriminant(&result.unwrap_err()), expected);
            assert_eq!(attempts, vec![Quality::UltraHiRes]);
        }
    }

    #[test]
    fn remote_body_diagnostic_never_echoes_payload() {
        let marker = "jwt=secret&request_sig=signed-value";
        let diagnostic = redacted_body_diagnostic(marker);
        assert_eq!(diagnostic, " : <response body omitted>");
        assert!(!diagnostic.contains(marker));
        assert!(!diagnostic.contains("secret"));
        assert_eq!(redacted_body_diagnostic("  \n"), " : <empty body>");
    }

    /// Install the process-level rustls `CryptoProvider`, once.
    ///
    /// UNRELATED DRIVE-BY FIX, 2026-08-16: the two gate tests below were failing
    /// on every run — `reqwest` is built with `rustls-tls-webpki-roots-no-provider`,
    /// so BUILDING a client panics with "No provider set" until a provider is
    /// installed, and a test binary has no application startup to do it. The
    /// application installs it via `qbz_app::ensure_crypto_provider`, but this
    /// crate sits below `qbz-app` and cannot call that, so it installs its own.
    /// `install_default` is idempotent and returns `Err` if someone else won the
    /// race, which is why the result is discarded.
    fn ensure_crypto_provider() {
        use std::sync::Once;
        static INIT: Once = Once::new();
        INIT.call_once(|| {
            let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        });
    }

    #[tokio::test]
    async fn delegated_credentials_use_the_validated_secret_not_the_first_candidate() {
        ensure_crypto_provider();
        let client = QobuzClient::new().expect("client construction is local-only");
        let app_id = "123456789";
        let first_candidate = "first-unvalidated-secret";
        let selected_secret = "second-validated-secret";

        *client.tokens.write().await = Some(BundleTokens {
            app_id: app_id.to_string(),
            secrets: vec![first_candidate.to_string(), selected_secret.to_string()],
            private_key: Some("owner-oauth-private-key".to_string()),
        });
        *client.validated_secret.write().await = Some(ValidatedBundleSecret::new(
            app_id.to_string(),
            selected_secret.to_string(),
        ));
        *client.session.write().await = Some(UserSession {
            user_auth_token: "owner-user-auth-token".to_string(),
            user_id: 7,
            email: "owner@example.test".to_string(),
            display_name: "Owner".to_string(),
            subscription_label: "Studio".to_string(),
            country_code: Some("MX".to_string()),
            language_code: Some("es".to_string()),
            ..UserSession::default()
        });

        let credentials = client.delegated_app_credentials().await.unwrap();
        let (delegated_app_id, delegated_secret) = credentials.values_for_test();
        assert_eq!(delegated_app_id, app_id);
        assert_eq!(delegated_secret, selected_secret);
        assert_ne!(delegated_secret, first_candidate);
        assert_ne!(delegated_secret, "owner-oauth-private-key");
        assert_ne!(delegated_secret, "owner-user-auth-token");
    }

    #[test]
    fn validated_secret_is_bound_to_its_bundle_generation() {
        let validated =
            ValidatedBundleSecret::new("123456789".to_string(), "selected-secret".to_string());
        let matching = BundleTokens {
            app_id: "123456789".to_string(),
            secrets: vec!["other-secret".to_string(), "selected-secret".to_string()],
            private_key: None,
        };
        let rotated_secret = BundleTokens {
            app_id: "123456789".to_string(),
            secrets: vec!["replacement-secret".to_string()],
            private_key: None,
        };
        let rotated_app = BundleTokens {
            app_id: "987654321".to_string(),
            secrets: vec!["selected-secret".to_string()],
            private_key: None,
        };

        assert!(validated.is_current_for(&matching));
        assert!(!validated.is_current_for(&rotated_secret));
        assert!(!validated.is_current_for(&rotated_app));
    }

    #[tokio::test]
    async fn delegated_credentials_require_an_initialized_owner_client() {
        ensure_crypto_provider();
        let client = QobuzClient::new().expect("client construction is local-only");

        assert!(matches!(
            client.delegated_app_credentials().await,
            Err(ApiError::BundleExtractionError(message))
                if message == "Client not initialized"
        ));
    }

    /// With the offline gate closed, any public API method must fail fast
    /// with the typed `ApiError::OfflineMode` — no network access, no
    /// connect timeout. The gate is process-global and tests run in
    /// parallel, so the shared lock serializes gate-touching tests and the
    /// drop guard reopens the gate even if the test panics.
    #[tokio::test]
    async fn offline_gate_fails_fast_with_typed_error() {
        ensure_crypto_provider();
        let _lock = crate::offline_gate::test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _reset = crate::offline_gate::TestGateReset;

        crate::offline_gate::set_offline(true);

        let client = QobuzClient::new().expect("client construction is local-only");
        let started = std::time::Instant::now();
        let result = client.get_album_suggest("0060254735180").await;
        let elapsed = started.elapsed();

        let err = result.err().expect("offline gate must fail the request");
        assert!(
            matches!(err, ApiError::OfflineMode),
            "expected ApiError::OfflineMode, got: {err}"
        );
        // OfflineMode must never be retried by the retry layer.
        assert!(!err.is_transient(), "OfflineMode must be non-transient");
        // Fail-fast: well under the 10s connect timeout — proves no network.
        assert!(
            elapsed < std::time::Duration::from_secs(1),
            "offline gate took {elapsed:?} — it must not touch the network"
        );
    }

    /// The sign-in methods are EXEMPT from the offline gate: user-initiated
    /// authentication is an explicit intent to reach Qobuz. On an
    /// uninitialized client each method gets PAST the closed gate and fails
    /// on the missing bundle tokens instead — never `ApiError::OfflineMode`,
    /// and without touching the network.
    #[tokio::test]
    async fn offline_gate_exempts_login_methods() {
        ensure_crypto_provider();
        let _lock = crate::offline_gate::test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let _reset = crate::offline_gate::TestGateReset;

        crate::offline_gate::set_offline(true);

        let client = QobuzClient::new().expect("client construction is local-only");

        let err = client
            .login("user@example.com", "pw")
            .await
            .err()
            .expect("uninitialized client must fail");
        assert!(
            !matches!(err, ApiError::OfflineMode),
            "login must bypass the offline gate, got: {err}"
        );

        let err = client
            .login_with_oauth_code("code")
            .await
            .err()
            .expect("uninitialized client must fail");
        assert!(
            !matches!(err, ApiError::OfflineMode),
            "login_with_oauth_code must bypass the offline gate, got: {err}"
        );

        let err = client
            .login_with_token("token")
            .await
            .err()
            .expect("uninitialized client must fail");
        assert!(
            !matches!(err, ApiError::OfflineMode),
            "login_with_token must bypass the offline gate, got: {err}"
        );
    }
}

#[cfg(test)]
#[path = "saved_session_tests.rs"]
mod saved_session_tests;
