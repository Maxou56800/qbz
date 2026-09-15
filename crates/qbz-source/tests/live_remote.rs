//! END TO END against real servers: sweep -> cache -> claim -> tracks -> meta
//! -> artwork -> playback -> actually fetch the bytes.
//!
//! ```text
//! QBZ_JELLYFIN_URL=… QBZ_JELLYFIN_USER=… QBZ_JELLYFIN_PASS=… \
//! QBZ_SUBSONIC_URL=… QBZ_SUBSONIC_USER=… QBZ_SUBSONIC_PASS=… \
//! cargo test -p qbz-source --test live_remote -- --nocapture
//! ```
//!
//! The unit tests in `sources/` prove each method in isolation against a
//! hand-made row. They cannot prove the thing that actually broke this project
//! once: that the pieces are CONNECTED — that a sweep writes rows a `claim` can
//! find, that the id a queue carries resolves back to a server id, and that the
//! url a ticket hands out returns audio rather than an error envelope.
//!
//! So each test below walks the whole chain and ends by pulling real bytes off
//! the server through the url the source produced. Nothing is asserted from a
//! type; everything is asserted from a server's answer.

use std::sync::{Arc, Once};

use qbz_media_cache::{CachedTrack, RemoteSource};
use qbz_source::{
    ArtRef, ArtSize, ItemKind, JellyfinCreds, JellyfinSource, PlaybackTicket, RawRef, Source,
    SourceId, SubsonicCreds, SubsonicSource,
};

fn install_crypto_provider() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    });
}

fn env3(a: &str, b: &str, c: &str) -> Option<(String, String, String)> {
    let v = (
        std::env::var(a).ok()?,
        std::env::var(b).ok()?,
        std::env::var(c).ok()?,
    );
    install_crypto_provider();
    Some(v)
}

/// A `QueueTrack` the way the frontend builds one. `qbz_models::QueueTrack` has
/// no `Default` on purpose — `streamable` defaults to TRUE through serde and a
/// derived `Default` would quietly make it false.
fn queue_track(id: u64, hint: &str, source: &str) -> qbz_models::QueueTrack {
    qbz_models::QueueTrack {
        id,
        title: String::new(),
        version: None,
        artist: String::new(),
        album: String::new(),
        album_version: None,
        duration_secs: 0,
        artwork_url: None,
        hires: false,
        bit_depth: None,
        sample_rate: None,
        is_local: true,
        album_id: None,
        artist_id: None,
        streamable: true,
        source: Some(source.into()),
        parental_warning: false,
        source_item_id_hint: (!hint.is_empty()).then(|| hint.to_string()),
        context_kind: None,
        context_id: None,
        isrc: None,
        recording_mbid: None,
    }
}

/// Follow a playback url and assert the server really hands back audio.
///
/// A Range request, because that is what the progressive feeder issues: it
/// checks seekability AND keeps the test to 64 KB instead of a 100 MB FLAC.
async fn assert_serves_audio(url: &str, label: &str) {
    let resp = reqwest::Client::new()
        .get(url)
        .header("Range", "bytes=0-65535")
        .send()
        .await
        .unwrap_or_else(|e| panic!("{label}: request failed: {e}"));
    assert_eq!(
        resp.status().as_u16(),
        206,
        "{label}: the server refused a Range request (the feeder needs one)"
    );
    let chunked = resp
        .headers()
        .get(reqwest::header::TRANSFER_ENCODING)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|te| te.eq_ignore_ascii_case("chunked"));
    assert!(!chunked, "{label}: chunked response — the server is TRANSCODING");
    let ctype = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let bytes = resp.bytes().await.expect("body");
    assert_eq!(bytes.len(), 65536, "{label}: short read");
    assert!(
        qbz_subsonic::looks_like_audio(Some(&ctype), bytes.len()),
        "{label}: the body is an error envelope, not audio ({ctype})"
    );
    eprintln!("  {label}: 206 {ctype}, {} bytes", bytes.len());
}

// ---------------------------------------------------------------------------
// Jellyfin
// ---------------------------------------------------------------------------

struct JfCreds(String, String);
impl JellyfinCreds for JfCreds {
    fn is_enabled(&self) -> bool {
        true
    }
    fn server(&self) -> Option<(String, String)> {
        Some((self.0.clone(), self.1.clone()))
    }
}

#[tokio::test]
async fn jellyfin_walks_from_a_sweep_to_real_audio() {
    let Some((url, user, pass)) = env3("QBZ_JELLYFIN_URL", "QBZ_JELLYFIN_USER", "QBZ_JELLYFIN_PASS")
    else {
        eprintln!("SKIP: QBZ_JELLYFIN_* not set");
        return;
    };

    // --- sweep ONE page off the real server ------------------------------
    let session = qbz_jellyfin::authenticate(&url, "qbz-e2e", &user, &pass)
        .await
        .expect("authenticate");
    let client =
        qbz_jellyfin::JellyfinClient::new(&url, &session.access_token, &session.user_id, "qbz-e2e").unwrap();
    let libs = client.music_libraries().await.expect("libraries");
    let (page, total) = client
        .tracks_page(Some(&libs[0].id), 0, None)
        .await
        .expect("page");
    eprintln!("jellyfin: swept {} of {total} rows", page.len());

    // --- into the cache, through the SAME mapping the sync will use ------
    let dir = tempfile::tempdir().unwrap();
    let rows: Vec<CachedTrack> = page
        .iter()
        .map(|t| CachedTrack {
            id: 0,
            source: "jellyfin".into(),
            item_id: t.id.clone(),
            server_id: session.server_id.clone(),
            library_id: libs[0].id.clone(),
            title: t.title.clone(),
            artist: t.artist.clone(),
            album_artist: t.album_artist.clone(),
            album: t.album.clone(),
            album_id: t.album_id.clone(),
            track_number: t.track_number,
            disc_number: t.disc_number,
            duration_ms: t.duration_ms,
            year: t.year,
            genres: t.genres.clone(),
            genre: t.genre.clone(),
            container: t.container.clone(),
            codec: t.codec.clone(),
            bit_depth: t.bit_depth,
            sample_rate_hz: t.sample_rate_hz,
            channels: t.channels,
            bitrate_kbps: t.bitrate_bps.map(|b| b / 1000),
            // Item art preserves a disc-specific cover. The album endpoint is
            // still addressable without its optional cache-busting tag.
            artwork_token: t
                .item_image_tag
                .as_ref()
                .map(|tag| format!("{}/{}", t.id, tag)),
            collection_artwork_token: (!t.album_id.is_empty()).then(|| {
                format!(
                    "{}/{}",
                    t.album_id,
                    t.album_image_tag.as_deref().unwrap_or_default()
                )
            }),
            isrc: None,
            recording_mbid: None,
            size_bytes: None,
        })
        .collect();
    {
        let mut conn = qbz_media_cache::open(&dir.path().join("remote_cache.db")).unwrap();
        qbz_media_cache::save_tracks(&mut conn, RemoteSource::Jellyfin, &rows).unwrap();
    }

    // --- the SOURCE, bound exactly as the app binds it -------------------
    let src = JellyfinSource::new();
    src.set_creds(Some(Arc::new(JfCreds(
        url.clone(),
        session.access_token.clone(),
    ))));
    src.bind_user(1, dir.path());

    let seed = rows
        .iter()
        .find(|r| r.artwork_token.is_some() && r.container == "flac")
        .or_else(|| rows.first())
        .expect("a row");

    // 1. ALBUM: claim -> tracks -> meta
    let album = src
        .claim(&RawRef {
            source: Some(SourceId::JELLYFIN),
            kind: Some(ItemKind::Album),
            id: seed.album_id.clone(),
            ..Default::default()
        })
        .expect("jellyfin claims its own album")
        .expect("a valid album ref");
    let tracks = src.tracks(&album).await.expect("album tracks");
    assert!(!tracks.is_empty(), "the album resolved to no tracks");
    let meta = src.meta(&album).await.expect("album meta");
    assert!(!meta.title.is_empty());
    eprintln!(
        "  album {:?} -> {} tracks, tier {:?}",
        meta.title,
        tracks.len(),
        meta.quality.tier()
    );

    // 2. ARTWORK: a fetchable, tokenless url that really returns an image.
    match src.artwork(&album, ArtSize::Card) {
        ArtRef::Fetch { url: art, cache_key } => {
            assert_eq!(art, cache_key, "a stable jellyfin url should key on itself");
            assert!(
                !art.contains(&session.access_token),
                "the token leaked into a cover url"
            );
            let r = reqwest::get(&art).await.expect("cover");
            assert!(r.status().is_success(), "cover fetch: {}", r.status());
            let ct = r
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            assert!(ct.starts_with("image/"), "cover returned {ct}");
            eprintln!("  cover: {ct}");
        }
        other => panic!("expected a fetchable cover, got {other:?}"),
    }

    // 3. THE ROUND TRIP THAT MATTERS. A queue row exactly as
    //    `cached_to_queue_track` builds it — namespaced id plus the server id
    //    in the hint — must claim back to the server's id.
    let qrow = tracks
        .iter()
        .find(|t| t.source_item_id_hint.is_some())
        .expect("a queue row");
    let claimed = src
        .claim(&RawRef::from_queue_track(qrow))
        .expect("jellyfin claims its own queue row")
        .expect("a valid track ref");
    assert_eq!(claimed.source(), SourceId::JELLYFIN);
    assert_eq!(
        claimed.id(),
        qrow.source_item_id_hint.as_deref().unwrap(),
        "the queue row did not resolve back to the server's id"
    );

    // ...and the same id resolves with NO hint, through the cache — the path a
    // producer that only kept the numeric id takes.
    let by_number = src
        .claim(&RawRef::from_queue_track(&queue_track(qrow.id, "", "jellyfin")))
        .expect("recognised by namespace bit")
        .expect("resolved through the cache");
    assert_eq!(by_number.id(), claimed.id());

    // 4. PLAYBACK: the ticket, then the actual bytes.
    match src.playback(&claimed, qrow).await.expect("playback") {
        PlaybackTicket::Stream {
            url: stream,
            log_tag,
            ..
        } => {
            assert_eq!(log_tag, "JELLYFIN");
            assert!(stream.contains("static=true"));
            assert_serves_audio(&stream, "jellyfin").await;
        }
        other => panic!("expected Stream, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Subsonic
// ---------------------------------------------------------------------------

struct SubCreds(String, qbz_subsonic::Credentials);
impl SubsonicCreds for SubCreds {
    fn is_enabled(&self) -> bool {
        true
    }
    fn server(&self) -> Option<(String, qbz_subsonic::Credentials)> {
        Some((self.0.clone(), self.1.clone()))
    }
}

#[tokio::test]
async fn subsonic_walks_from_a_sweep_to_real_audio() {
    let Some((url, user, pass)) = env3("QBZ_SUBSONIC_URL", "QBZ_SUBSONIC_USER", "QBZ_SUBSONIC_PASS")
    else {
        eprintln!("SKIP: QBZ_SUBSONIC_* not set");
        return;
    };

    let creds = qbz_subsonic::Credentials::new(&user, &pass, "qbze2e");
    let client = qbz_subsonic::SubsonicClient::new(&url, creds.clone()).unwrap();
    let info = client.ping().await.expect("ping");
    eprintln!(
        "subsonic: {} (openSubsonic={})",
        info.kind, info.open_subsonic
    );
    let page = client.search_page(0).await.expect("page");
    eprintln!("subsonic: swept {} rows", page.len());

    let dir = tempfile::tempdir().unwrap();
    let rows: Vec<CachedTrack> = page
        .iter()
        .map(|t| CachedTrack {
            id: 0,
            source: "subsonic".into(),
            item_id: t.id.clone(),
            server_id: String::new(),
            library_id: String::new(),
            title: t.title.clone(),
            artist: t.artist.clone(),
            album_artist: t.album_artist.clone(),
            album: t.album.clone(),
            album_id: t.album_id.clone(),
            track_number: t.track_number,
            disc_number: t.disc_number,
            duration_ms: t.duration_ms,
            year: t.year,
            genres: t.genres.clone(),
            genre: t.genre.clone(),
            container: t.suffix.clone(),
            codec: t.content_type.clone(),
            bit_depth: t.bit_depth,
            sample_rate_hz: t.sample_rate_hz,
            channels: t.channels,
            bitrate_kbps: t.bitrate_kbps,
            // The OPAQUE coverArt id, verbatim.
            artwork_token: t.cover_art.clone(),
            collection_artwork_token: None,
            isrc: None,
            recording_mbid: None,
            size_bytes: t.size,
        })
        .collect();
    {
        let mut conn = qbz_media_cache::open(&dir.path().join("remote_cache.db")).unwrap();
        qbz_media_cache::save_tracks(&mut conn, RemoteSource::Subsonic, &rows).unwrap();
    }

    let src = SubsonicSource::new();
    src.set_creds(Some(Arc::new(SubCreds(url.clone(), creds.clone()))));
    src.bind_user(1, dir.path());

    let seed = rows
        .iter()
        .find(|r| r.artwork_token.is_some() && r.container == "flac")
        .or_else(|| rows.first())
        .expect("a row");

    let album = src
        .claim(&RawRef {
            source: Some(SourceId::SUBSONIC),
            kind: Some(ItemKind::Album),
            id: seed.album_id.clone(),
            ..Default::default()
        })
        .expect("subsonic claims its own album")
        .expect("a valid album ref");
    let tracks = src.tracks(&album).await.expect("album tracks");
    assert!(!tracks.is_empty());
    let meta = src.meta(&album).await.expect("album meta");
    eprintln!(
        "  album {:?} -> {} tracks, tier {:?}",
        meta.title,
        tracks.len(),
        meta.quality.tier()
    );

    // The cover url CARRIES credentials, so its key must not be the url.
    match src.artwork(&album, ArtSize::Card) {
        ArtRef::Fetch { url: art, cache_key } => {
            assert_ne!(art, cache_key, "a credentialed url must not be its own key");
            assert!(
                !cache_key.contains("t="),
                "a credential reached the cache key"
            );
            let r = reqwest::get(&art).await.expect("cover");
            let ct = r
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            assert!(ct.starts_with("image/"), "cover returned {ct}");
            eprintln!("  cover: {ct}, key {cache_key}");
        }
        other => panic!("expected a fetchable cover, got {other:?}"),
    }

    let qrow = tracks.first().expect("a queue row");
    let claimed = src
        .claim(&RawRef::from_queue_track(qrow))
        .expect("subsonic claims its own queue row")
        .expect("a valid track ref");
    assert_eq!(
        claimed.id(),
        qrow.source_item_id_hint.as_deref().unwrap(),
        "the queue row did not resolve back to the server's id"
    );
    // A row stamped with the SERVER'S BRAND resolves too.
    let branded = src
        .claim(&RawRef::from_queue_track(&queue_track(
            qrow.id,
            claimed.id(),
            "navidrome",
        )))
        .expect("a navidrome-stamped row is still Subsonic")
        .expect("resolved");
    assert_eq!(branded.id(), claimed.id());

    match src.playback(&claimed, qrow).await.expect("playback") {
        PlaybackTicket::Stream {
            url: stream,
            log_tag,
            ..
        } => {
            assert_eq!(log_tag, "SUBSONIC");
            assert!(stream.contains("format=raw"));
            assert_serves_audio(&stream, "subsonic").await;
        }
        other => panic!("expected Stream, got {other:?}"),
    }
}

/// The registry must route each namespace to its owner, and no source may claim
/// a neighbour's row. Needs no server, and it is the assertion that would catch
/// an id-namespace mistake before it played the wrong track.
#[test]
fn the_registry_routes_each_namespace_to_its_owner() {
    let registry = qbz_source::SourceRegistry::with_defaults();
    for (src, word) in [
        (RemoteSource::Jellyfin, "jellyfin"),
        (RemoteSource::Subsonic, "subsonic"),
    ] {
        let id = src.namespace(7) as u64;
        let claimed = registry
            .claim(&RawRef::from_queue_track(&queue_track(
                id,
                "server-side-id",
                word,
            )))
            .unwrap_or_else(|e| panic!("{word} row was not claimed: {e}"));
        assert_eq!(claimed.source().as_str(), word);
        assert_eq!(claimed.id(), "server-side-id");
    }
    // And a Plex-namespaced id still goes to Plex, not to a newcomer.
    let plex = registry
        .claim(&RawRef::from_queue_track(&queue_track(
            (1u64 << 40) | 44_440,
            "44440",
            "plex",
        )))
        .expect("plex row");
    assert_eq!(plex.source(), SourceId::PLEX);
}
