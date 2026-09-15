//! Jellyfin / Subsonic glue for this frontend.
//!
//! The Qt-side sibling of `local_plex`, and deliberately smaller than it: the
//! persisted model lives in `qbz_app::settings::media_servers` and every
//! runtime concern lives in `qbz-jellyfin` / `qbz-subsonic` / `qbz-source`
//! (ADR-006). This file owns three things only:
//!
//!  1. a process-global `MediaServerState` bound to the ACTIVE user
//!     (`<data_dir>/qbz/users/<uid>/media_servers.db`);
//!  2. the gates the Local Library reads — [`configured_words`] and
//!     [`remote_cache_path`], which together decide whether a remote row
//!     appears in the grid at all;
//!  3. the identifiers whose STABILITY is load-bearing — Jellyfin's
//!     `DeviceId` and Subsonic's salt — generated once and persisted.
//!
//! The credential glue that `qbz-source` consumes lives in `source_wiring`
//! beside the rest of the registry's injections, so there is one place to look
//! when a source answers "not configured".

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex, RwLock};

use qbz_app::settings::media_servers::{MediaServerKind, MediaServerSettings, MediaServerState};
use qbz_app::user_data::UserDataPaths;

static STATE: LazyLock<MediaServerState> = LazyLock::new(MediaServerState::new);
static BOUND: Mutex<Option<u64>> = Mutex::new(None);

/// The last read per server, so [`get`] is not a database hit.
///
/// SAME defect as `local_plex`'s, introduced the same way: `local_rows::art_ref`
/// asks the owning source per ROW what an artwork token means, and both media
/// sources answer by looking at whether they are connected. Without this, every
/// remote row in the grid costs a mutex plus a SQLite query — measured at
/// 39-92 µs/row against 2 µs for a row whose source needs no credentials.
///
/// Invalidated by [`put`], [`disconnect`], [`init_for_user`] and [`reset`], so
/// a connect or a master-toggle flip is still seen on the very next read.
static CACHE: LazyLock<RwLock<HashMap<&'static str, MediaServerSettings>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

fn invalidate_cache() {
    if let Ok(mut c) = CACHE.write() {
        c.clear();
    }
}

/// Bind lazily as well as from the auth callback. The derived catalog starts
/// after the first paint and can race session restoration; reading an unbound
/// store there used to answer "disabled" for configured servers, while a
/// stale process-global cache could answer with the previous user's servers.
fn ensure_bound() {
    let Some(uid) = UserDataPaths::load_last_user_id() else {
        return;
    };
    let mut bound = BOUND.lock().unwrap_or_else(|error| error.into_inner());
    if *bound == Some(uid) {
        return;
    }
    crate::media_connection_qt::cancel_all();
    invalidate_cache();
    STATE.reset();
    let dir = dirs::data_dir()
        .unwrap_or_default()
        .join("qbz")
        .join("users")
        .join(uid.to_string());
    STATE.init_at(&dir);
    *bound = Some(uid);
    log::info!("[qbz-qt] media server settings bound to user {uid}");
}

/// Bind the store to the active user. Called from `auth_qt`.
pub fn init_for_user(base_dir: &std::path::Path) {
    crate::media_connection_qt::cancel_all();
    invalidate_cache();
    STATE.init_at(base_dir);
    *BOUND.lock().unwrap_or_else(|error| error.into_inner()) =
        UserDataPaths::load_last_user_id();
    // Mint the two stable identifiers ONCE, now, rather than at first use.
    // Both are load-bearing and both are easy to get wrong lazily: a Jellyfin
    // DeviceId minted per connection attempt revokes the previous token, and a
    // salt minted per request re-downloads every cover.
    ensure_identities();
}

pub fn reset() {
    crate::media_connection_qt::cancel_all();
    invalidate_cache();
    *BOUND.lock().unwrap_or_else(|error| error.into_inner()) = None;
    STATE.reset();
}

pub fn get(kind: MediaServerKind) -> MediaServerSettings {
    ensure_bound();
    if let Ok(c) = CACHE.read() {
        if let Some(s) = c.get(kind.as_str()) {
            return s.clone();
        }
    }
    let fresh = STATE.get(kind);
    if let Ok(mut c) = CACHE.write() {
        c.insert(kind.as_str(), fresh.clone());
    }
    fresh
}

pub fn put(kind: MediaServerKind, s: &MediaServerSettings) {
    STATE.put(kind, s);
    invalidate_cache();
}

pub fn disconnect(kind: MediaServerKind) {
    let _sync_gate = crate::media_sync_qt::media_server_state_guard(kind);
    crate::media_sync_qt::cancel(kind);
    STATE.disconnect(kind);
    invalidate_cache();
}

/// Which remote sources the Local Library union may show.
///
/// ENABLED **and** finished configuring — see
/// `MediaServerSettings::is_configured`. A server that is toggled on but has no
/// credentials yet would widen the union's `source IN (…)` over rows nothing
/// ever swept.
pub fn configured_words() -> Vec<&'static str> {
    // Through the CACHED `get`, not `STATE.configured_words()` — this is called
    // once per albums-page load and twice more via `remote_cache_path`.
    MediaServerKind::ALL
        .iter()
        .filter(|k| get(**k).is_configured(**k))
        .map(|k| k.as_str())
        .collect()
}

/// The shared remote mirror for the active user, or `None` when no remote
/// source is configured.
///
/// `None` short-circuits the ATTACH in `get_albums_metadata_page`: a user with
/// no media server should not pay for a database open on every page.
pub fn remote_cache_path() -> Option<PathBuf> {
    if configured_words().is_empty() {
        return None;
    }
    let uid = qbz_app::user_data::UserDataPaths::load_last_user_id().unwrap_or(0);
    Some(
        dirs::data_dir()?
            .join("qbz")
            .join("users")
            .join(uid.to_string())
            .join("remote_cache.db"),
    )
}

/// Generate-once, persist-forever identifiers.
///
/// **Jellyfin's `DeviceId`.** The server keys its SESSION on it, and a second
/// `AuthenticateByName` under the same one REVOKES the previous token —
/// measured against 10.11.11:
///
/// ```text
/// same DeviceId:      auth -> T1, auth -> T2;  T1 = 401, T2 = 200
/// different DeviceId: auth -> A,  auth -> B;   A  = 200, B  = 200
/// ```
///
/// So it must be stable per INSTALL: a constant shared by every QBZ would make
/// two installs log each other out, and a fresh one per launch drops the
/// previous run's token and litters the server's devices page.
///
/// **Subsonic's salt.** `t = md5(password + salt)`, and the salt travels in
/// clear — it is not a secret. Fixing it per install is deliberate: rolling it
/// per request would make every cover URL unique, and the artwork cache keys on
/// the URL, so each pass would re-download every cover.
fn ensure_identities() {
    let mut jf = STATE.get(MediaServerKind::Jellyfin);
    if jf.device_id.is_empty() {
        jf.device_id = format!("qbz-{}", random_hex(16));
        STATE.put(MediaServerKind::Jellyfin, &jf);
        log::info!("[qbz-qt] jellyfin: minted a device id for this install");
    }
    let mut sub = STATE.get(MediaServerKind::Subsonic);
    if sub.salt.is_empty() {
        sub.salt = random_hex(8);
        STATE.put(MediaServerKind::Subsonic, &sub);
        log::info!("[qbz-qt] subsonic: minted a salt for this install");
    }
}

/// `len` hex characters of process-and-time entropy.
///
/// Not cryptographic, and it does not need to be: both consumers want
/// UNIQUENESS across installs, not unpredictability. The salt is transmitted in
/// clear by the protocol itself, and the device id is a label the server files
/// a session under. Pulling in an RNG crate for two strings that are generated
/// once per install would be more dependency than the job.
fn random_hex(len: usize) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut out = String::with_capacity(len);
    let mut seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
        ^ (std::process::id() as u64) << 32;
    while out.len() < len {
        let mut h = DefaultHasher::new();
        seed.hash(&mut h);
        seed = h.finish();
        out.push_str(&format!("{seed:016x}"));
    }
    out.truncate(len);
    out
}

/// The `Credentials` a Subsonic call needs, derived from the stored password
/// and the install's fixed salt.
pub fn subsonic_credentials() -> Option<(String, qbz_subsonic::Credentials)> {
    let s = STATE.get(MediaServerKind::Subsonic);
    if !s.is_configured(MediaServerKind::Subsonic) {
        return None;
    }
    Some((
        s.base_url.clone(),
        qbz_subsonic::Credentials::new(&s.username, &s.password, &s.salt),
    ))
}

/// `(base_url, access_token)` when Jellyfin is usable right now.
pub fn jellyfin_server() -> Option<(String, String)> {
    let s = STATE.get(MediaServerKind::Jellyfin);
    s.is_configured(MediaServerKind::Jellyfin)
        .then(|| (s.base_url.clone(), s.token.clone()))
}

// ---------------------------------------------------------------------------
// Connect / probe / purge — the settings panel's verbs
// ---------------------------------------------------------------------------

/// "Test connection", BEFORE the user is asked for a password.
///
/// Returns the server's display name. The point is to separate "wrong address"
/// from "wrong password": without it, every failure looks like bad credentials
/// and the user retypes a password that was never the problem.
///
/// Jellyfin has an unauthenticated probe (`/System/Info/Public`), so its test is
/// genuinely credential-free. Subsonic has none — `ping.view` needs credentials
/// — so its "test" can only confirm the address is reachable, and it reports
/// that honestly rather than claiming more.
pub async fn probe(kind: MediaServerKind, url: &str) -> Result<String, String> {
    match kind {
        MediaServerKind::Jellyfin => qbz_jellyfin::probe(url)
            .await
            .map(|i| format!("{} {}", i.server_name, i.version))
            .map_err(|e| e.to_string()),
        MediaServerKind::Subsonic => {
            // No credential-free endpoint exists. A `ping` with an empty user
            // returns the protocol's own "missing parameter" — which still
            // proves a Subsonic server answered at this address, and that is
            // exactly what the test is for.
            let creds = qbz_subsonic::Credentials::new("", "", "probe");
            let client =
                qbz_subsonic::SubsonicClient::new(url, creds).map_err(|e| e.to_string())?;
            match client.ping().await {
                Ok(i) => Ok(format!("{} {}", i.kind, i.version)),
                // A protocol-level error means a Subsonic server IS there.
                Err(qbz_subsonic::SubsonicError::Unauthorized)
                | Err(qbz_subsonic::SubsonicError::Api { .. }) => {
                    Ok("a Subsonic server (sign in to continue)".to_string())
                }
                Err(e) => Err(e.to_string()),
            }
        }
    }
}

/// Authenticate and verify library access; return settings for a guarded commit.
///
/// What gets stored differs per protocol, and the asymmetry is the protocol's:
/// Jellyfin issues a token and never needs the password again; Subsonic has no
/// session, so the password is kept and its token is re-derived per request.
async fn verified_jellyfin_session(
    url: &str,
    session: qbz_jellyfin::Session,
    mut cfg: MediaServerSettings,
) -> Result<MediaServerSettings, String> {
    qbz_jellyfin::JellyfinClient::new(url, &session.access_token, &session.user_id, &cfg.device_id)
        .map_err(|e| e.to_string())?
        .music_libraries()
        .await
        .map_err(|e| {
            qbz_i18n::t_args(
                "Signed in, but library access failed: {}",
                &[&e.to_string()],
            )
        })?;
    let info = tokio::time::timeout(std::time::Duration::from_secs(3), qbz_jellyfin::probe(url))
        .await
        .ok()
        .and_then(Result::ok);
    cfg.base_url = qbz_jellyfin::normalize_base_url(url);
    cfg.token = session.access_token;
    // The USER ID, not the typed name: every `/Items` call keys on it,
    // and the two are not interchangeable.
    cfg.username = session.user_id;
    cfg.server_id = session.server_id;
    cfg.server_name = info.map(|i| i.server_name).unwrap_or_default();
    // The password is deliberately NOT stored — the token replaced it.
    cfg.password = String::new();
    cfg.enabled = true;
    Ok(cfg)
}

pub async fn quick_connect(
    url: &str,
    cfg: MediaServerSettings,
    operation: &crate::media_connection_qt::Operation,
) -> Result<MediaServerSettings, String> {
    let pairing = qbz_jellyfin::QuickConnect::start(url, &cfg.device_id)
        .await
        .map_err(quick_connect_error)?;
    operation.phase("pairing-waiting", "");
    operation.pairing_code(pairing.code());
    let session = pairing
        .wait_for_authorization(std::time::Duration::from_secs(300))
        .await
        .map_err(quick_connect_error)?;
    operation.phase("pairing-verifying", "");
    verified_jellyfin_session(url, session, cfg).await
}
fn quick_connect_error(error: qbz_jellyfin::JellyfinError) -> String {
    match error {
        qbz_jellyfin::JellyfinError::QuickConnectDisabled => {
            qbz_i18n::t("Quick Connect is disabled on this server.")
        }
        qbz_jellyfin::JellyfinError::QuickConnectExpired => {
            qbz_i18n::t("Quick Connect expired. Request a new code.")
        }
        other => qbz_i18n::t_args("Quick Connect failed: {}", &[&other.to_string()]),
    }
}

pub async fn connect(
    kind: MediaServerKind,
    url: &str,
    username: &str,
    password: &str,
    mut cfg: MediaServerSettings,
    operation: &crate::media_connection_qt::Operation,
) -> Result<MediaServerSettings, String> {
    match kind {
        MediaServerKind::Jellyfin => {
            // The DeviceId must be the PERSISTED one: authenticating under a
            // fresh id every time revokes the previous token and litters the
            // server's device list. `init_for_user` minted it.
            let session = qbz_jellyfin::authenticate(url, &cfg.device_id, username, password)
                .await
                .map_err(|e| qbz_i18n::t_args("Sign-in failed: {}", &[&e.to_string()]))?;
            operation.phase("verifying", "");
            return verified_jellyfin_session(url, session, cfg).await;
        }
        MediaServerKind::Subsonic => {
            let creds = qbz_subsonic::Credentials::new(username, password, &cfg.salt);
            let client =
                qbz_subsonic::SubsonicClient::new(url, creds).map_err(|e| e.to_string())?;
            let info = client
                .ping()
                .await
                .map_err(|e| qbz_i18n::t_args("Sign-in failed: {}", &[&e.to_string()]))?;
            operation.phase("verifying", "");
            client.album_ids(0).await.map_err(|e| {
                qbz_i18n::t_args(
                    "Signed in, but library access failed: {}",
                    &[&e.to_string()],
                )
            })?;
            if !info.open_subsonic {
                // Not fatal: the library still lists and still plays. But
                // without the OpenSubsonic fields there is no bitDepth or
                // samplingRate, so every quality badge degrades to unknown —
                // and a user who cares about that deserves to hear it now
                // rather than wonder later.
                log::warn!(
                    "[qbz-qt] subsonic: {} is not OpenSubsonic — quality badges will be unavailable",
                    info.kind
                );
            }
            cfg.base_url = qbz_subsonic::normalize_base_url(url);
            cfg.username = username.to_string();
            // KEPT, because the protocol re-derives its token on every request.
            cfg.password = password.to_string();
            cfg.token = String::new();
            cfg.server_name = format!("{} {}", info.kind, info.version);
        }
    }
    cfg.enabled = true;
    Ok(cfg)
}

/// Drop this server's cached rows.
///
/// Only from DISCONNECT. The master toggle deliberately keeps them: turning a
/// server off should be cheap to undo, and re-sweeping a 5000-track Jellyfin
/// library costs 46 seconds.
pub fn purge_cache(kind: MediaServerKind) {
    let source = match kind {
        MediaServerKind::Jellyfin => qbz_media_cache::RemoteSource::Jellyfin,
        MediaServerKind::Subsonic => qbz_media_cache::RemoteSource::Subsonic,
    };
    let handle = match kind {
        MediaServerKind::Jellyfin => qbz_source::registry().jellyfin().cache(),
        MediaServerKind::Subsonic => qbz_source::registry().subsonic().cache(),
    };
    match handle.with_mut(|c| qbz_media_cache::clear(c, source)) {
        Some(Ok(n)) => log::info!("[qbz-qt] {} cache purged: {n} rows", kind.as_str()),
        Some(Err(e)) => log::error!("[qbz-qt] {} cache purge failed: {e}", kind.as_str()),
        None => log::warn!("[qbz-qt] {} cache purge: no user bound", kind.as_str()),
    }
}

// ---------------------------------------------------------------------------
// The `LocalTrack` shape — what every Local Library surface consumes
// ---------------------------------------------------------------------------

/// Map a cached remote row into the `LocalTrack` shape the Local Library's
/// album page, per-disc menu, bulk bar and row cache all read.
///
/// The Plex sibling is `local_plex::map_cached_to_local_track`. Two fields are
/// decided differently and both matter:
///
/// - `file_path` carries the SERVER'S OWN id, not a path. A remote track has no
///   file on this machine, and this is the slot `local_queue_track` stamps into
///   `source_item_id_hint` — which is exactly what `claim` prefers, because a
///   server id survives a cache rebuild that renumbers rowids. Nothing ever
///   opens it as a path; `LocalSource` never sees these rows.
/// - `album_group_key` is the PREFIXED key (`jellyfin:<albumId>`), matching the
///   one the grid's SQL union publishes, so "go to album" from a track row
///   lands on the same page the card opens.
/// Bulk media-cache resolution for playlist refs: `(source, item_id, _)`
/// rows to `(source, item_id) -> LocalTrack`. Misses are simply absent —
/// the caller renders them as honest Unresolved rows. Blocking.
pub fn tracks_by_item_ids_blocking(
    refs: &[(String, String, i32)],
) -> std::collections::HashMap<(String, String), qbz_library::LocalTrack> {
    let mut out = std::collections::HashMap::new();
    for source_name in ["jellyfin", "subsonic"] {
        let items: Vec<&str> = refs
            .iter()
            .filter(|(s, _, _)| s == source_name)
            .map(|(_, item, _)| item.as_str())
            .collect();
        if items.is_empty() {
            continue;
        }
        let (remote, read_source) = if source_name == "jellyfin" {
            (qbz_media_cache::RemoteSource::Jellyfin, source_name)
        } else {
            (qbz_media_cache::RemoteSource::Subsonic, source_name)
        };
        let read = |conn: &rusqlite::Connection| {
            items
                .iter()
                .filter_map(|item| {
                    qbz_media_cache::track_by_item_id(conn, remote, item)
                        .ok()
                        .flatten()
                        .map(cached_to_local_track)
                        .map(|track| ((read_source.to_string(), (*item).to_string()), track))
                })
                .collect::<Vec<_>>()
        };
        let resolved = match remote {
            qbz_media_cache::RemoteSource::Jellyfin => qbz_source::registry()
                .jellyfin()
                .cache()
                .with(read)
                .unwrap_or_default(),
            qbz_media_cache::RemoteSource::Subsonic => qbz_source::registry()
                .subsonic()
                .cache()
                .with(read)
                .unwrap_or_default(),
        };
        out.extend(resolved);
    }
    out
}

pub fn cached_to_local_track(t: qbz_media_cache::CachedTrack) -> qbz_library::LocalTrack {
    let source = t.source.clone();
    let source_instance = if t.server_id.trim().is_empty() {
        "default".to_string()
    } else {
        t.server_id.clone()
    };
    let album_id = t.album_id.clone();
    let mut track = qbz_library::LocalTrack {
        id: t.id,
        file_path: t.item_id,
        title: t.title,
        artist: t.artist.clone(),
        album_artist: (!t.album_artist.is_empty()).then_some(t.album_artist),
        album: t.album.clone(),
        album_group_key: format!("{}:{}", t.source, t.album_id),
        album_group_title: t.album,
        track_number: t.track_number,
        disc_number: t.disc_number,
        duration_secs: t.duration_ms / 1000,
        format: parse_audio_format(&t.container, t.codec.as_deref()),
        bit_depth: t.bit_depth,
        sample_rate: t.sample_rate_hz.unwrap_or(0) as f64,
        year: t.year,
        artwork_path: t.artwork_token,
        collection_artwork_path: t.collection_artwork_token,
        source: Some(t.source),
        ..Default::default()
    };
    crate::remote_metadata_qt::apply(&mut track, &source, &source_instance, &album_id);
    track
}

/// Container/codec name -> the enum the quality badge derives from.
///
/// Both are consulted because the two protocols disagree about which one is
/// populated: Jellyfin reports a `Container` and often a codec, Subsonic
/// reports a `suffix` and a MIME type.
fn parse_audio_format(container: &str, codec: Option<&str>) -> qbz_library::AudioFormat {
    use qbz_library::AudioFormat as F;
    let probe = |s: &str| -> Option<F> {
        let s = s.to_ascii_lowercase();
        // DSD first, and it matters MORE here than in the sibling parsers:
        // this one falls back to `Flac`, not `Unknown`, so an unrecognised DSD
        // track from Jellyfin/Navidrome does not merely lose its format — it
        // is actively mislabelled lossless-PCM, with a "24-bit / …" badge it
        // never had. Jellyfin reports `Container: "dsf"`, Subsonic a `suffix`
        // of `dsf`/`dff` and a MIME of `audio/x-dsf`.
        if s.contains("dsd") || s.contains("dsf") || s.contains("dff") {
            Some(F::Dsd)
        } else if s.contains("flac") {
            Some(F::Flac)
        } else if s.contains("mp3") || s.contains("mpeg") {
            Some(F::Mp3)
        } else if s.contains("alac") || s.contains("m4a") || s.contains("mp4") {
            Some(F::Alac)
        } else if s.contains("wav") {
            Some(F::Wav)
        } else if s.contains("aiff") {
            Some(F::Aiff)
        } else {
            None
        }
    };
    probe(container)
        .or_else(|| codec.and_then(probe))
        .unwrap_or(F::Flac)
}

/// The tracks of one remote album, by its PREFIXED grid key
/// (`jellyfin:<albumId>` / `subsonic:<albumId>`), in disc/track order.
///
/// `None` when the key is not a remote one, so the caller falls through to
/// `library.db` exactly as before.
pub fn album_tracks(prefixed_key: &str) -> Option<Vec<qbz_library::LocalTrack>> {
    let (word, album_id) = prefixed_key.split_once(':')?;
    let source = qbz_media_cache::RemoteSource::from_word(word)?;
    let (kind, handle) = match source {
        qbz_media_cache::RemoteSource::Jellyfin => (
            MediaServerKind::Jellyfin,
            qbz_source::registry().jellyfin().cache(),
        ),
        qbz_media_cache::RemoteSource::Subsonic => (
            MediaServerKind::Subsonic,
            qbz_source::registry().subsonic().cache(),
        ),
    };
    if !get(kind).is_configured(kind) {
        // It is still a recognised remote key, so return Some(empty) instead
        // of falling through to the local database — but never open the
        // disabled source's cache.
        return Some(Vec::new());
    }
    let rows = handle
        .with(|c| qbz_media_cache::album_tracks(c, source, album_id).unwrap_or_default())
        .unwrap_or_default();
    if source == qbz_media_cache::RemoteSource::Jellyfin {
        crate::media_sync_qt::prioritize_jellyfin_quality(
            rows.iter().map(|row| row.item_id.clone()).collect(),
            false,
        );
    }
    Some(rows.into_iter().map(cached_to_local_track).collect())
}

/// Exact cached counts for enabled sources, in Jellyfin/Subsonic order.
pub fn cached_track_counts() -> (i64, i64) {
    let count = |kind: MediaServerKind| -> i64 {
        if !get(kind).is_configured(kind) {
            return 0;
        }
        let (source, handle) = match kind {
            MediaServerKind::Jellyfin => (
                qbz_media_cache::RemoteSource::Jellyfin,
                qbz_source::registry().jellyfin().cache(),
            ),
            MediaServerKind::Subsonic => (
                qbz_media_cache::RemoteSource::Subsonic,
                qbz_source::registry().subsonic().cache(),
            ),
        };
        handle
            .with(|c| qbz_media_cache::count(c, source).unwrap_or(0) as i64)
            .unwrap_or(0)
    };
    (
        count(MediaServerKind::Jellyfin),
        count(MediaServerKind::Subsonic),
    )
}

pub fn cached_track_count() -> i64 {
    let (jellyfin, subsonic) = cached_track_counts();
    jellyfin + subsonic
}

/// Remote artist aggregates without materialising every cached track.
pub fn cached_artists() -> Vec<(&'static str, qbz_media_cache::CachedArtist)> {
    let mut out = Vec::new();
    for kind in MediaServerKind::ALL {
        if !get(kind).is_configured(kind) {
            continue;
        }
        let (source, handle) = match kind {
            MediaServerKind::Jellyfin => (
                qbz_media_cache::RemoteSource::Jellyfin,
                qbz_source::registry().jellyfin().cache(),
            ),
            MediaServerKind::Subsonic => (
                qbz_media_cache::RemoteSource::Subsonic,
                qbz_source::registry().subsonic().cache(),
            ),
        };
        if let Some(rows) = handle.with(|c| qbz_media_cache::artists(c, source).unwrap_or_default())
        {
            out.extend(rows.into_iter().map(|row| (kind.as_str(), row)));
        }
    }
    out
}

/// One source-local candidate page for the global Tracks merge.
pub fn search_tracks_page(
    source_word: &str,
    query: &str,
    offset: u64,
    limit: u64,
    sort: &str,
    formats: &[String],
    other_formats: bool,
    quality_tiers: &[String],
) -> Vec<qbz_library::LocalTrack> {
    let Some(kind) = MediaServerKind::from_word(source_word) else {
        return Vec::new();
    };
    if !get(kind).is_configured(kind) {
        return Vec::new();
    }
    let (source, handle) = match kind {
        MediaServerKind::Jellyfin => (
            qbz_media_cache::RemoteSource::Jellyfin,
            qbz_source::registry().jellyfin().cache(),
        ),
        MediaServerKind::Subsonic => (
            qbz_media_cache::RemoteSource::Subsonic,
            qbz_source::registry().subsonic().cache(),
        ),
    };
    let rows = handle
        .with(|c| {
            qbz_media_cache::search_page_filtered(
                c,
                source,
                query,
                offset,
                limit,
                sort,
                formats,
                other_formats,
                quality_tiers,
            )
            .unwrap_or_default()
        })
        .unwrap_or_default();
    if source == qbz_media_cache::RemoteSource::Jellyfin {
        crate::media_sync_qt::prioritize_jellyfin_quality(
            rows.iter().map(|row| row.item_id.clone()).collect(),
            false,
        );
    }
    rows.into_iter().map(cached_to_local_track).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Exercise actual HTTP requests without touching configured servers or settings.
    async fn fixture(
        responses: Vec<(&'static str, u16, &'static str)>,
    ) -> (String, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            for (path, status, body) in responses {
                let (mut socket, _) =
                    tokio::time::timeout(std::time::Duration::from_secs(5), listener.accept())
                        .await
                        .unwrap()
                        .unwrap();
                let mut bytes = Vec::new();
                loop {
                    let mut buf = [0; 2048];
                    let n = socket.read(&mut buf).await.unwrap();
                    assert!(n > 0);
                    bytes.extend_from_slice(&buf[..n]);
                    if let Some(end) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                        let header = String::from_utf8_lossy(&bytes[..end]);
                        let length = header
                            .lines()
                            .find_map(|l| {
                                l.to_ascii_lowercase()
                                    .strip_prefix("content-length:")
                                    .and_then(|v| v.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if bytes.len() >= end + 4 + length {
                            break;
                        }
                    }
                }
                let request = String::from_utf8_lossy(&bytes);
                assert!(request.lines().next().unwrap().contains(path));
                if path.contains("Views") {
                    // Jellyfin 12 reads only the modern form (#502).
                    let lower = request.to_ascii_lowercase();
                    assert!(lower.contains("authorization: mediabrowser "));
                    assert!(request.contains(r#"Token="test-token""#));
                    assert!(request.contains(r#"DeviceId="test-device""#));
                    assert!(!lower.contains("x-emby-token"));
                }
                let response = format!("HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());
                socket.write_all(response.as_bytes()).await.unwrap();
            }
        });
        (url, task)
    }

    #[tokio::test]
    async fn connection_distinguishes_login_from_library_rejection_for_both_providers() {
        qbz_app::ensure_crypto_provider();
        const JF_AUTH: &str =
            r#"{"AccessToken":"test-token","ServerId":"server","User":{"Id":"uid","Name":"user"}}"#;
        const SUB_OK: &str = r#"{"subsonic-response":{"status":"ok","version":"1.16.1","type":"navidrome","openSubsonic":true,"albumList2":{"album":[]}}}"#;
        const SUB_NO: &str =
            r#"{"subsonic-response":{"status":"failed","error":{"code":40,"message":"denied"}}}"#;
        for kind in MediaServerKind::ALL {
            // "success-10.8": a server that predates `/UserViews` (10.9) must
            // still connect through the obsolete route. Self-hosted servers
            // often sit on stable distributions and lag behind releases.
            for mode in [
                "login-rejected",
                "library-rejected",
                "success",
                "success-10.8",
            ] {
                let responses = match (kind, mode) {
                    (MediaServerKind::Jellyfin, "login-rejected") => {
                        vec![("/Users/AuthenticateByName", 401, "{}")]
                    }
                    (MediaServerKind::Jellyfin, "library-rejected") => vec![
                        ("/Users/AuthenticateByName", 200, JF_AUTH),
                        ("/UserViews?userId=uid", 401, "{}"),
                    ],
                    (MediaServerKind::Jellyfin, "success-10.8") => vec![
                        ("/Users/AuthenticateByName", 200, JF_AUTH),
                        ("/UserViews?userId=uid", 404, ""),
                        (
                            "/Users/uid/Views",
                            200,
                            r#"{"Items":[],"TotalRecordCount":0}"#,
                        ),
                        (
                            "/System/Info/Public",
                            200,
                            r#"{"ServerName":"test","Version":"10.8.13","Id":"server"}"#,
                        ),
                    ],
                    (MediaServerKind::Jellyfin, _) => vec![
                        ("/Users/AuthenticateByName", 200, JF_AUTH),
                        (
                            "/UserViews?userId=uid",
                            200,
                            r#"{"Items":[],"TotalRecordCount":0}"#,
                        ),
                        (
                            "/System/Info/Public",
                            200,
                            r#"{"ServerName":"test","Version":"10.11","Id":"server"}"#,
                        ),
                    ],
                    (MediaServerKind::Subsonic, "login-rejected") => {
                        vec![("/rest/ping.view", 200, SUB_NO)]
                    }
                    (MediaServerKind::Subsonic, "library-rejected") => vec![
                        ("/rest/ping.view", 200, SUB_OK),
                        ("/rest/getAlbumList2.view", 200, SUB_NO),
                    ],
                    (MediaServerKind::Subsonic, _) => vec![
                        ("/rest/ping.view", 200, SUB_OK),
                        ("/rest/getAlbumList2.view", 200, SUB_OK),
                    ],
                };
                let (url, server) = fixture(responses).await;
                let operation = crate::media_connection_qt::Operation::isolated(kind);
                let cfg = MediaServerSettings {
                    device_id: "test-device".into(),
                    salt: "test-salt".into(),
                    ..Default::default()
                };
                let result = connect(kind, &url, "user", "password", cfg, &operation).await;
                server.await.unwrap();
                match mode {
                    "login-rejected" => assert!(result.unwrap_err().contains("Sign-in failed")),
                    "library-rejected" => assert!(result
                        .unwrap_err()
                        .contains("Signed in, but library access failed")),
                    _ => {
                        let cfg = result.unwrap();
                        assert!(cfg.enabled);
                        if kind == MediaServerKind::Jellyfin {
                            assert_eq!(cfg.token, "test-token");
                            assert!(cfg.password.is_empty());
                        } else {
                            assert_eq!(cfg.password, "password");
                        }
                    }
                }
                drop(operation);
            }
        }
    }

    /// The identifiers must be unique per install. They are generated once and
    /// persisted, so the only property worth pinning here is that two draws do
    /// not collide — a fixed string would silently make every install share a
    /// Jellyfin session.
    #[test]
    fn the_generated_identifiers_are_the_right_length_and_differ() {
        let a = random_hex(16);
        let b = random_hex(16);
        assert_eq!(a.len(), 16);
        assert_eq!(random_hex(8).len(), 8);
        assert_ne!(
            a, b,
            "two draws collided — every install would share a session"
        );
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
