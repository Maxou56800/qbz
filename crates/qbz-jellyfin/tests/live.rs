//! LIVE tests against a real Jellyfin server. Skipped unless the environment
//! names one, so CI and a laptop on a train both stay green.
//!
//!   QBZ_JELLYFIN_URL=http://192.168.0.69:8096 \
//!   QBZ_JELLYFIN_USER=admin QBZ_JELLYFIN_PASS=... \
//!   cargo test -p qbz-jellyfin --test live -- --nocapture
//!
//! These exist because the unit tests above them cannot fail the way this
//! integration really fails. A pure mapper test proves the JSON shape is
//! understood; it cannot prove the endpoint still exists, that the token still
//! opens it, or that `?static=true` still hands back the original bytes. That
//! last one is the audio contract, and the only honest way to check it is to
//! ask a server.

use std::env;
use std::sync::Once;

use tokio::sync::OnceCell;

/// reqwest is built `...-no-provider`, so a `reqwest::Client` cannot be built
/// until a process-level rustls CryptoProvider exists. In the app that is
/// `qbz_app::install_crypto_provider`; here there is no app.
fn install_crypto_provider() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    });
}

fn server() -> Option<(String, String, String)> {
    Some((
        env::var("QBZ_JELLYFIN_URL").ok()?,
        env::var("QBZ_JELLYFIN_USER").ok()?,
        env::var("QBZ_JELLYFIN_PASS").ok()?,
    ))
}

/// ONE session for the whole test binary.
///
/// Not an optimisation — a correctness fix, and the reason is a real property
/// of the protocol rather than a test artifact. Jellyfin keys its session on
/// `DeviceId`, and re-authenticating with the SAME one **revokes the previous
/// token**. Measured directly against 10.11.11:
///
/// ```text
/// same DeviceId:      auth -> T1, auth -> T2;  T1 = 401, T2 = 200
/// different DeviceId: auth -> A,  auth -> B;   A  = 200, B  = 200
/// ```
///
/// So five tests authenticating in parallel under one DeviceId spend their time
/// invalidating each other, and whichever finishes first is left holding a dead
/// token — which is exactly what the `Unauthorized` on the second call was. The
/// app carries the same obligation: authenticate ONCE, hold the token, and keep
/// the DeviceId stable per install.
static SESSION: OnceCell<qbz_jellyfin::Session> = OnceCell::const_new();

async fn session(url: &str, user: &str, pass: &str) -> &'static qbz_jellyfin::Session {
    SESSION
        .get_or_init(|| async {
            qbz_jellyfin::authenticate(url, "qbz-live-test", user, pass)
                .await
                .expect("authenticate")
        })
        .await
}

/// The authenticated client every live test shares.
async fn client(url: &str, user: &str, pass: &str) -> qbz_jellyfin::JellyfinClient {
    let s = session(url, user, pass).await;
    qbz_jellyfin::JellyfinClient::new(url, &s.access_token, &s.user_id, "qbz-live-test").unwrap()
}

macro_rules! skip_without_server {
    () => {
        match server() {
            Some(v) => {
                install_crypto_provider();
                v
            }
            None => {
                eprintln!("SKIP: set QBZ_JELLYFIN_URL / _USER / _PASS to run the live tests");
                return;
            }
        }
    };
}

#[tokio::test]
async fn probe_reports_a_server_without_credentials() {
    let (url, _, _) = skip_without_server!();
    let info = qbz_jellyfin::probe(&url).await.expect("probe");
    eprintln!("server: {} {}", info.server_name, info.version);
    assert!(!info.id.is_empty());
    assert!(info.startup_wizard_completed);
}

#[tokio::test]
async fn a_full_round_trip_yields_tracks_with_real_quality() {
    let (url, user, pass) = skip_without_server!();
    let s = session(&url, &user, &pass).await;
    assert!(!s.access_token.is_empty());

    let c = client(&url, &user, &pass).await;

    let libs = c.music_libraries().await.expect("libraries");
    assert!(!libs.is_empty(), "the server exposes no music library");
    let lib = &libs[0];
    eprintln!("library: {} ({})", lib.name, lib.id);

    let total = c.track_count(Some(&lib.id)).await.expect("count");
    assert!(total > 0, "the music library is empty");

    let (page, reported) = c.tracks_page(Some(&lib.id), 0, None).await.expect("page");
    assert_eq!(reported, total, "the count and the page disagree");
    assert!(!page.is_empty());

    // The point of `Fields=MediaSources`: SOME row must carry a real bit depth,
    // or the quality badge silently degrades to "unknown" for a whole library.
    let with_depth = page.iter().filter(|t| t.bit_depth.is_some()).count();
    assert!(
        with_depth > 0,
        "no row in a {}-track page carried a bit depth — MediaSources is not being requested",
        page.len()
    );
    // ...and a lossy row must NOT invent one.
    for t in page.iter().filter(|t| t.container == "mp3") {
        assert_eq!(t.bit_depth, None, "an mp3 reported a bit depth: {}", t.title);
    }
    // Every row must be identifiable and groupable, or the cache cannot key it.
    for t in &page {
        assert!(!t.id.is_empty(), "a row arrived with no id");
        assert!(!t.album_id.is_empty(), "row {} has no album id", t.title);
    }
    eprintln!(
        "page: {} rows, {} with a bit depth, {} total in library",
        page.len(),
        with_depth,
        total
    );
}

/// THE audio contract. `?static=true` must answer with the ORIGINAL bytes:
/// a `Content-Length`, `Accept-Ranges`, and a 206 to a Range request. A
/// chunked answer means the server decided to transcode, which for a hi-res
/// track is exactly the failure this app cannot ship.
#[tokio::test]
async fn the_stream_endpoint_is_direct_and_seekable() {
    let (url, user, pass) = skip_without_server!();
    let c = client(&url, &user, &pass).await;
    let libs = c.music_libraries().await.expect("libraries");
    let (page, _) = c
        .tracks_page(Some(&libs[0].id), 0, None)
        .await
        .expect("page");
    // A LOSSLESS row, so the assertion is about the case that matters.
    let track = page
        .iter()
        .find(|t| t.container == "flac")
        .or_else(|| page.first())
        .expect("a track");

    let http = reqwest::Client::new();
    let resp = http
        .get(c.stream_url(&track.id))
        .header("Range", "bytes=0-65535")
        .send()
        .await
        .expect("range request");

    assert_eq!(resp.status().as_u16(), 206, "the server refused a Range request");
    let te = resp
        .headers()
        .get(reqwest::header::TRANSFER_ENCODING)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());
    assert!(
        te.as_deref() != Some("chunked"),
        "chunked response — the server is TRANSCODING, not direct-playing"
    );
    let ctype = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let bytes = resp.bytes().await.expect("body");
    assert_eq!(bytes.len(), 65536, "short read on a Range request");
    if track.container == "flac" {
        assert_eq!(&bytes[..4], b"fLaC", "the body is not a FLAC bitstream");
    }
    eprintln!(
        "stream: {} {} -> 206, {} bytes, {ctype}",
        track.container,
        track.title,
        bytes.len()
    );
}

/// Cover urls carry no credentials, and the server really serves them that way.
#[tokio::test]
async fn cover_art_needs_no_token() {
    let (url, user, pass) = skip_without_server!();
    let c = client(&url, &user, &pass).await;
    let libs = c.music_libraries().await.expect("libraries");
    let (page, _) = c
        .tracks_page(Some(&libs[0].id), 0, None)
        .await
        .expect("page");
    let track = page
        .iter()
        .find(|t| t.album_image_tag.is_some())
        .expect("no row in the first page has an album cover");

    let art = qbz_jellyfin::image_url(
        c.base_url(),
        &track.album_id,
        track.album_image_tag.as_deref(),
        qbz_jellyfin::IMAGE_PX,
    );
    let token = &session(&url, &user, &pass).await.access_token;
    assert!(!art.contains(token), "the cover url leaked the token");

    let resp = reqwest::Client::new().get(&art).send().await.expect("cover");
    assert!(resp.status().is_success(), "cover fetch failed: {}", resp.status());
    let ctype = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    assert!(ctype.starts_with("image/"), "cover url returned {ctype}");
    let bytes = resp.bytes().await.expect("cover body");
    assert!(bytes.len() > 1024, "cover is {} bytes", bytes.len());
    eprintln!("cover: {ctype}, {} bytes", bytes.len());
}

/// The delta sweep really filters. Without this, an incremental re-scan would
/// silently be a full one.
#[tokio::test]
async fn a_future_delta_returns_nothing() {
    let (url, user, pass) = skip_without_server!();
    let c = client(&url, &user, &pass).await;
    let libs = c.music_libraries().await.expect("libraries");
    let (page, _) = c
        .tracks_page(Some(&libs[0].id), 0, Some("2999-01-01T00:00:00Z"))
        .await
        .expect("delta page");
    assert!(page.is_empty(), "a year-2999 delta returned {} rows", page.len());
}
