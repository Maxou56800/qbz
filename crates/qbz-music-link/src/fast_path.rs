//! Direct platform metadata (bypass Odesli for speed).
//!
//! Ported verbatim from the `src-tauri` link resolver. For Tidal/Deezer this
//! calls the platform API directly; for Spotify it scrapes the embed page.
//! Apple Music has no direct API and falls through to Odesli.

use crate::detection::{spotify, MusicProvider};

/// Public Cloudflare-worker proxy base. NOT a secret — this is the same public
/// URL hardcoded in the `src-tauri` original. No API keys are embedded here.
const QBZ_PROXY_BASE: &str = "https://qbz-api-proxy.blitzkriegfc.workers.dev";

/// Same budget as `odesli.rs` (`REQUEST_TIMEOUT`). A bare
/// `reqwest::Client::new()` has NO timeout, so a black-holed connection hung
/// the fast path forever — and since `lib.rs` only falls back to Odesli after
/// the fast path returns, the link never resolved.
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Build the fast-path client with a total deadline of `total`.
pub(crate) fn build_client(total: std::time::Duration) -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(total)
        .connect_timeout(CONNECT_TIMEOUT.min(total))
        .user_agent("QBZ/1.0.0")
        .build()
        .expect("reqwest client")
}

/// The one shared client (connection pool + timeouts) for the direct
/// platform calls and the QBZ proxy.
fn client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| build_client(REQUEST_TIMEOUT))
}

/// Try to get title+artist directly from the platform API.
/// Returns None if the platform isn't supported or the request fails.
pub(crate) async fn try_direct_platform_metadata(
    url: &str,
    provider: &MusicProvider,
    is_track: bool,
) -> Option<(String, String)> {
    match provider {
        MusicProvider::Deezer => try_deezer_metadata(url, is_track).await,
        MusicProvider::Spotify => try_spotify_metadata(url, is_track).await,
        MusicProvider::Tidal => try_tidal_metadata(url, is_track).await,
        MusicProvider::AppleMusic => None, // No direct API available
    }
}

/// Extract a numeric or alphanumeric ID after /track/ or /album/ in a URL.
fn extract_entity_id(url: &str, entity_type: &str) -> Option<String> {
    let pattern = format!("/{}/", entity_type);
    let idx = url.find(&pattern)?;
    let rest = &url[idx + pattern.len()..];
    let id = rest.split(['?', '/', '#']).next()?;
    if id.is_empty() {
        None
    } else {
        Some(id.to_string())
    }
}

/// Extract Spotify ID from URL or URI.
fn extract_spotify_entity_id(url: &str, entity_type: &str) -> Option<String> {
    // URI format: spotify:track:abc123
    let uri_pattern = format!("spotify:{}:", entity_type);
    if let Some(rest) = url.strip_prefix(&uri_pattern) {
        let id = rest.split(['?', '/']).next()?;
        if !id.is_empty() {
            return Some(id.to_string());
        }
    }
    extract_entity_id(url, entity_type)
}

async fn try_deezer_metadata(url: &str, is_track: bool) -> Option<(String, String)> {
    let entity = if is_track { "track" } else { "album" };
    let id = extract_entity_id(url, entity).or_else(|| {
        if is_track {
            None
        } else {
            extract_entity_id(url, "track")
        }
    })?;
    let api_url = format!("https://api.deezer.com/{}/{}", entity, id);

    log::debug!("Link resolver: Deezer direct API: {}", api_url);
    let data: serde_json::Value = reqwest::get(&api_url).await.ok()?.json().await.ok()?;
    if data.get("error").is_some() {
        return None;
    }

    let title = data.get("title")?.as_str()?.to_string();
    let artist = data
        .get("artist")
        .and_then(|a| a.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Some((title, artist))
}

async fn try_spotify_metadata(url: &str, is_track: bool) -> Option<(String, String)> {
    let entity = if is_track { "track" } else { "album" };
    let id = extract_spotify_entity_id(url, entity)?;

    log::debug!("Link resolver: Spotify embed scrape for {} {}", entity, id);
    spotify::fetch_embed_metadata(entity, &id).await
}

async fn try_tidal_metadata(url: &str, is_track: bool) -> Option<(String, String)> {
    let entity = if is_track { "track" } else { "album" };
    let id = extract_entity_id(url, entity)
        // Also try /browse/track/ pattern
        .or_else(|| extract_entity_id(url, &format!("browse/{}", entity)))?;
    let token = get_proxy_token("tidal").await?;
    let api_url = format!(
        "https://openapi.tidal.com/v2/{}s/{}?countryCode=US&include=artists",
        entity, id
    );

    log::debug!("Link resolver: Tidal direct API: {}", api_url);
    let data: serde_json::Value = client()
        .get(&api_url)
        .header("Authorization", format!("Bearer {}", token))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;

    let title = data
        .get("data")
        .and_then(|d| d.get("attributes"))
        .and_then(|a| a.get("title"))
        .and_then(|v| v.as_str())?
        .to_string();

    // Artist name is in the "included" array
    let artist = data
        .get("included")
        .and_then(|v| v.as_array())
        .and_then(|arr| {
            arr.iter()
                .find(|item| item.get("type").and_then(|v| v.as_str()) == Some("artists"))
        })
        .and_then(|item| item.get("attributes"))
        .and_then(|a| a.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    Some((title, artist))
}

/// Get an OAuth token from the QBZ proxy for the given platform.
async fn get_proxy_token(platform: &str) -> Option<String> {
    let url = format!("{}/{}/token", QBZ_PROXY_BASE, platform);
    let data: serde_json::Value = client()
        .get(&url)
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    data.get("access_token")
        .and_then(|v| v.as_str())
        .map(|v| v.to_string())
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    /// A silent server (accepts, never answers) must not park the fast path:
    /// the Odesli fallback only runs after it returns.
    #[tokio::test]
    async fn fast_path_client_gives_up_on_a_silent_server() {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/tidal/token", listener.local_addr().unwrap());
        let hold = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            tokio::time::sleep(Duration::from_secs(30)).await;
            drop(socket);
        });

        let started = Instant::now();
        let err = super::build_client(Duration::from_millis(400))
            .get(&url)
            .send()
            .await
            .unwrap_err();

        assert!(err.is_timeout(), "expected a timeout, got: {err}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "took {:?}",
            started.elapsed()
        );
        hold.abort();
    }
}
