//! Integrations settings + runtime controller — the Slint
//! `crates/qbz/src/scrobble.rs` + `discord_rpc.rs` + the settings.rs
//! integration rows, ported onto the SAME stores:
//! `scrobbler_settings.db` (`qbz_app::settings::scrobblers`, per-user),
//! `discover_prefs.db` (`qbz_app::settings::discover_prefs`, per-user), the
//! shared per-user `offline_settings.db` `scrobble_queue`, the shared
//! `cache/listenbrainz_v2.db` (credentials + `listen_queue`) and the shared
//! `ui_prefs.json` (`musicbrainz_enabled`, `discord_rpc_enabled`).
//!
//! Everything here is STRICTLY OPT-IN (the project rule): no client is
//! constructed and no request leaves the process until the user connected the
//! service AND its enable flags are on. `ScrobblerSettings::lastfm_active()` /
//! `listenbrainz_active()` (master + per-service + authed) gate every fire,
//! and `DiscordRpc` only opens its IPC socket once `set_enabled(true)` came
//! from the persisted opt-in or the toggle.
//!
//! Inventory (1:1 with `settings/IntegrationsSettings.slint`):
//!   - Recommendations (discover_prefs `show_recommendations`)
//!   - MusicBrainz (`ui_prefs.musicbrainz_enabled` -> `core.musicbrainz_set_enabled`)
//!   - Scrobblers master (+ collapse) -> Last.fm, ListenBrainz
//!   - Discord Rich Presence (`ui_prefs.discord_rpc_enabled`)
//! (Discogs is NOT a settings integration in either reference — it is a tag
//! editor remote-metadata provider + an album external link.)
//!
//! Runtime wiring (the fire path) needs two call sites OUTSIDE this file:
//! [`start`] at shell entry and [`on_track_changed`] / [`discord_push`] on the
//! playback track-change + play/pause edges. See the "GLUE" notes on those
//! functions.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use qbz_app::offline_mode::{
    Connectivity, OfflineMode, OfflineModeSettings, OfflineModeStore, OfflineStatus,
};
use qbz_app::scrobble_timing::scrobble_delay_secs;
use qbz_app::settings::discover_prefs::DiscoverPrefsStore;
use qbz_app::settings::scrobblers::{ScrobblerSettings, ScrobblerSettingsState};
use qbz_app::shell::AppRuntime;
use qbz_core::LoggingAdapter;
use qbz_integrations::discord::DiscordRpc;
use qbz_integrations::lastfm::LastFmClient;
use qbz_integrations::listenbrainz::cache::ListenBrainzCache;
use qbz_integrations::listenbrainz::{AdditionalInfo, ListenBrainzClient};
use qbz_integrations::NowListening;
use qbz_models::QueueTrack;

use crate::playback_qt::{
    begin_owner_action, begin_owner_action_exact, OwnerActionToken,
};
use crate::settings_qt::{pref_bool, save_pref};

// ---------------------------------------------------------------------------
// Stores (SAME files as the Slint app)
// ---------------------------------------------------------------------------

static SCROBBLE: OnceLock<ScrobblerSettingsState> = OnceLock::new();
static SCROBBLE_DIR: Mutex<Option<PathBuf>> = Mutex::new(None);
/// Qobuz authentication is independent of a Last.fm/ListenBrainz session.
/// False covers both the login screen and an explicitly started offline shell.
static QOBUZ_AUTHENTICATED: AtomicBool = AtomicBool::new(false);

fn scrobble() -> &'static ScrobblerSettingsState {
    SCROBBLE.get_or_init(ScrobblerSettingsState::new_empty)
}

/// Bind the independent scrobbler credentials to the active/last Qobuz
/// profile. Unlike the old sidebar-derived OnceLock, this can rebind on an
/// account switch. Logout deliberately keeps the last binding: that is what
/// allows opted-in local playback to scrobble without a Qobuz session.
pub fn init_for_user(base_dir: &Path) {
    if let Err(e) = scrobble().init_at(base_dir) {
        log::warn!("[qbz-qt] scrobbler settings store unavailable: {e}");
    } else {
        let changed = SCROBBLE_DIR
            .lock()
            .map(|mut dir| {
                let changed = dir.as_deref() != Some(base_dir);
                *dir = Some(base_dir.to_path_buf());
                changed
            })
            .unwrap_or(false);
        // A timer armed for account A must never submit through account B's
        // newly rebound credentials. Re-entering the same offline profile
        // keeps it.
        if changed {
            SCROBBLE_GEN.fetch_add(1, Ordering::SeqCst);
        }
    }

    // Discover preferences belong to the Qobuz profile even though the
    // scrobbler credentials above deliberately survive logout. Replace this
    // handle on every account activation; the old lazy-only initialization
    // pinned Recommendations visibility to the first user for the lifetime of
    // the process.
    let store = match DiscoverPrefsStore::new_at(base_dir) {
        Ok(store) => Some(store),
        Err(e) => {
            log::warn!("[qbz-qt] discover prefs store unavailable: {e}");
            None
        }
    };
    let cell = DISCOVER.get_or_init(|| Mutex::new(None));
    if let Ok(mut guard) = cell.lock() {
        *guard = store;
    }
}

fn scrobble_user_dir() -> Option<PathBuf> {
    SCROBBLE_DIR.lock().ok().and_then(|dir| dir.clone())
}

pub fn set_qobuz_authenticated(authenticated: bool) {
    if QOBUZ_AUTHENTICATED.swap(authenticated, Ordering::SeqCst) != authenticated {
        request_flush();
    }
}

static DISCOVER: OnceLock<Mutex<Option<DiscoverPrefsStore>>> = OnceLock::new();

fn with_discover<T>(f: impl FnOnce(&DiscoverPrefsStore) -> T) -> Option<T> {
    let cell = DISCOVER.get_or_init(|| {
        // Before the session restores there is no user directory yet; that
        // is the normal startup order, not a failure (`bind_qobuz_user`
        // replaces this handle on activation). Only a directory that exists
        // and still cannot open its store is worth a warning.
        let store = match crate::sidebar_qt::user_dir() {
            None => {
                log::debug!("[qbz-qt] discover prefs store not bound yet: no active user");
                None
            }
            Some(dir) => match DiscoverPrefsStore::new_at(&dir) {
                Ok(store) => Some(store),
                Err(e) => {
                    log::warn!("[qbz-qt] discover prefs store unavailable: {e}");
                    None
                }
            },
        };
        Mutex::new(store)
    });
    let guard = cell.lock().ok()?;
    guard.as_ref().map(f)
}

/// Drop only the Qobuz-profile binding. Scrobbler credentials intentionally
/// remain bound so opted-in local playback can continue to scrobble while the
/// user is logged out.
pub fn unbind_qobuz_user() {
    if let Some(cell) = DISCOVER.get() {
        if let Ok(mut guard) = cell.lock() {
            *guard = None;
        }
    }
}

static DISCORD: OnceLock<DiscordRpc> = OnceLock::new();

fn discord() -> &'static DiscordRpc {
    DISCORD.get_or_init(DiscordRpc::new)
}

/// `<user_dir>/cache/listenbrainz_v2.db` — the SAME per-user file the Slint
/// and Tauri builds open, so credentials AND the offline listen queue are
/// shared across frontends (scrobble.rs `listenbrainz_cache_path`).
fn listenbrainz_cache_path() -> Option<PathBuf> {
    let dir = scrobble_user_dir()?.join("cache");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("listenbrainz_v2.db"))
}

// ---------------------------------------------------------------------------
// Transient UI state (ScrobbleState busy/auth-url/status + the Last.fm
// pending request token — process memory only, like LASTFM_PENDING_TOKEN).
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct IntegrationUi {
    pub lastfm_busy: bool,
    pub listenbrainz_busy: bool,
    pub lastfm_auth_url: String,
    pub status_text: String,
    /// 0 none / 1 info / 2 ok / 3 error (ScrobbleState.status-kind).
    pub status_kind: i32,
    pending_lastfm_token: String,
}

static UI: OnceLock<Mutex<IntegrationUi>> = OnceLock::new();

fn ui_state() -> &'static Mutex<IntegrationUi> {
    UI.get_or_init(|| Mutex::new(IntegrationUi::default()))
}

pub fn ui_snapshot() -> IntegrationUi {
    let g = ui_state().lock().unwrap();
    IntegrationUi {
        lastfm_busy: g.lastfm_busy,
        listenbrainz_busy: g.listenbrainz_busy,
        lastfm_auth_url: g.lastfm_auth_url.clone(),
        status_text: g.status_text.clone(),
        status_kind: g.status_kind,
        pending_lastfm_token: String::new(),
    }
}

fn set_status(text: String, kind: i32) {
    let mut g = ui_state().lock().unwrap();
    g.status_text = text;
    g.status_kind = kind;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListenBrainzValidationStart {
    Started,
    Busy,
    MissingToken,
}

/// Acquire the ListenBrainz validation slot and publish its first honest UI
/// state atomically. The busy check deliberately precedes the empty-token
/// check: QML used to submit the real token on blur and an accidental empty
/// value on click, so the empty request overwrote "Validating..." while the
/// real request was still in flight.
fn begin_listenbrainz_validation(
    ui: &mut IntegrationUi,
    has_token: bool,
    validating_status: String,
    missing_status: String,
) -> ListenBrainzValidationStart {
    if ui.listenbrainz_busy {
        return ListenBrainzValidationStart::Busy;
    }
    if !has_token {
        ui.status_text = missing_status;
        ui.status_kind = 3;
        return ListenBrainzValidationStart::MissingToken;
    }
    ui.listenbrainz_busy = true;
    ui.status_text = validating_status;
    ui.status_kind = 1;
    ListenBrainzValidationStart::Started
}

fn finish_listenbrainz_validation(ui: &mut IntegrationUi, status: String, kind: i32) {
    ui.listenbrainz_busy = false;
    ui.status_text = status;
    ui.status_kind = kind;
}

// ---------------------------------------------------------------------------
// Snapshot fields (folded into SettingsDoc by settings_qt::publish_snapshot)
// ---------------------------------------------------------------------------

pub fn scrobble_settings() -> ScrobblerSettings {
    scrobble().get_settings().unwrap_or_default()
}

/// The playlist importer's PREFILL + optional credential (2.0.3 expansion).
///
/// Returns `(lastfm_username, listenbrainz_username, listenbrainz_token)`.
///
/// STRICTLY A CONVENIENCE. Both service sources read PUBLIC data with a bare
/// username, so nothing here is required — a user who has connected nothing
/// types a handle and it works. The token is the one credential that changes
/// behaviour when present (higher ListenBrainz rate limits, and the user's own
/// private playlists resolve), and it is sent only because it is already
/// stored; the importer never asks for it.
pub fn scrobbler_handles() -> (String, String, Option<String>) {
    let cfg = scrobble_settings();
    let token = Some(cfg.listenbrainz_token.clone()).filter(|t| !t.trim().is_empty());
    (cfg.lastfm_username, cfg.listenbrainz_username, token)
}

pub fn show_recommendations() -> bool {
    with_discover(|s| s.load().show_recommendations).unwrap_or(true)
}

pub fn discord_enabled() -> bool {
    pref_bool("discord_rpc_enabled", false)
}

// ---------------------------------------------------------------------------
// Shell entry (scrobble::start + discord_rpc::init + the MusicBrainz seed)
// ---------------------------------------------------------------------------

/// One-shot guard for the offline-engine flush watcher (lives for the process).
static FLUSH_WATCHER: OnceLock<()> = OnceLock::new();

/// Per-user runtime start. GLUE: call from `enter_shell` (main.rs), AFTER
/// [`init_for_user`] binds the scrobbler profile.
///
/// Applies the three persisted opt-ins that would otherwise only take effect
/// after the user re-toggled them:
///   - MusicBrainz -> `core.musicbrainz_set_enabled` (main.rs:9023 in Slint),
///   - Discord     -> `DiscordRpc::set_enabled` + an initial presence push
///                    (discord_rpc::init — the PR #477 "enabled on restart"
///                    fix: applied AFTER the session, never at early boot),
///   - ListenBrainz-> adopt the shared-cache credentials when this build has
///                    none (a Tauri/Slint sign-in carries over; enable flags
///                    are NOT touched, scrobbling stays opt-in).
/// Then drains the offline scrobble queues whenever the combined network /
/// manual-offline / logged-out policy becomes sendable.
pub fn start(runtime: &Arc<AppRuntime<LoggingAdapter>>) {
    // The auth fan-out bound this before shell entry; touch it now so a missing
    // binding degrades to the normal default snapshot rather than a later race.
    let _ = scrobble().get_settings();

    let rt = runtime.clone();
    crate::spawn(async move {
        // MusicBrainz opt-out is a plain pref; the core caches its own flag.
        let mb_on = pref_bool("musicbrainz_enabled", true);
        rt.core().musicbrainz_set_enabled(mb_on).await;

        seed_listenbrainz_from_shared_cache().await;

        let discord_on = discord_enabled();
        discord().set_enabled(discord_on);
        if discord_on {
            discord_push(&rt);
        }

        flush_if_allowed().await;
    });

    FLUSH_WATCHER.get_or_init(|| {
        crate::spawn(async move {
            let mut rx = crate::offline_fwd::engine().subscribe();
            let _ = *rx.borrow_and_update();
            let mut could_send = can_send_now(&scrobble_settings()).await;
            loop {
                if rx.changed().await.is_err() {
                    break;
                }
                let _ = *rx.borrow_and_update();
                let can_send = can_send_now(&scrobble_settings()).await;
                if !could_send && can_send {
                    log::info!("[qbz-qt] scrobblers: network policy allows flush");
                    flush_offline_queues().await;
                }
                could_send = can_send;
            }
        });
    });
}

/// Adopt the shared `ListenBrainzCache` credentials when this build's store
/// has no LB token yet (scrobble.rs `seed_listenbrainz_from_shared_cache`).
/// Enable flags are NOT touched — scrobbling stays opt-in per build.
async fn seed_listenbrainz_from_shared_cache() {
    if scrobble_settings().listenbrainz_is_authed() {
        return;
    }
    let Some(path) = listenbrainz_cache_path() else {
        return;
    };
    let creds = tokio::task::spawn_blocking(move || {
        ListenBrainzCache::new(&path).and_then(|c| c.get_credentials())
    })
    .await;
    if let Ok(Ok((Some(token), Some(user_name)))) = creds {
        if !token.is_empty() {
            log::info!("[qbz-qt] adopting ListenBrainz credentials from shared cache");
            if let Err(e) = scrobble().set_listenbrainz_token(&token, &user_name) {
                log::warn!("[qbz-qt] persist adopted ListenBrainz credentials failed: {e}");
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Toggle handlers (settings_qt.rs bool arms delegate here)
// ---------------------------------------------------------------------------

pub fn set_show_recommendations(value: bool) -> Result<(), String> {
    with_discover(|s| {
        let mut prefs = s.load();
        prefs.show_recommendations = value;
        s.save(&prefs)
    })
    .unwrap_or_else(|| Err("discover prefs store not open".to_string()))
}

pub fn set_scrobble_enabled(value: bool) -> Result<(), String> {
    let result = scrobble().set_enabled(value);
    if result.is_ok() && value {
        request_flush();
    }
    result
}

pub fn set_scrobble_collapsed(value: bool) -> Result<(), String> {
    scrobble().set_ui_collapsed(value)
}

pub fn set_logged_out_scrobbling(value: bool) -> Result<(), String> {
    let result = scrobble().set_allow_logged_out_scrobbling(value);
    if result.is_ok() && value {
        request_flush();
    }
    result
}

pub fn set_lastfm_enabled(value: bool) -> Result<(), String> {
    let result = scrobble().set_lastfm_enabled(value);
    if result.is_ok() && value {
        request_flush();
    }
    result
}

pub fn set_listenbrainz_enabled(value: bool) -> Result<(), String> {
    let result = scrobble().set_listenbrainz_enabled(value);
    if result.is_ok() && value {
        request_flush();
    }
    result
}

/// Settings > Offline policy changed. A newly enabled immediate path may make
/// queued listens deliverable without an offline-engine mode transition.
pub fn scrobble_policy_changed() {
    request_flush();
}

/// Discord toggle (discord_rpc::set_enabled): persist the opt-in, apply it to
/// the live client, and push the current track (enable) — `set_enabled(false)`
/// tears the IPC connection down, so the presence disappears immediately.
pub fn set_discord_enabled(value: bool) -> Result<(), String> {
    save_pref("discord_rpc_enabled", serde_json::json!(value));
    discord().set_enabled(value);
    if value {
        discord_push(&crate::app());
    }
    log::info!("[qbz-qt] discord_rpc_enabled -> {value}");
    Ok(())
}

pub fn set_musicbrainz_enabled(value: bool) -> Result<(), String> {
    save_pref("musicbrainz_enabled", serde_json::json!(value));
    Ok(())
}

// ---------------------------------------------------------------------------
// Discord Rich Presence (discord_rpc.rs)
// ---------------------------------------------------------------------------

/// Build the "now listening" snapshot from the live queue + playback state and
/// push it. No-op when the user has not opted in (cheap early return — nothing
/// is fetched, nothing connects).
///
/// GLUE: call on the playback track-change edge and on play/pause, mirroring
/// the Tauri service's (track_id, is_playing) transition pushes.
pub fn discord_push(runtime: &Arc<AppRuntime<LoggingAdapter>>) {
    if !discord().is_enabled() {
        return;
    }
    let Some(owner_action) = begin_owner_action() else {
        return;
    };
    let owner_token = owner_action.token();
    drop(owner_action);
    let runtime = runtime.clone();
    crate::spawn(async move {
        // The task has not read its queue input yet, so re-admit the exact
        // producer observation at the consumer. This also invalidates the
        // special pre-service token if QConnect appeared before scheduling.
        let Some(_owner_action) = begin_owner_action_exact(owner_token).await else {
            return;
        };
        push_discord_current(&runtime, None, None).await;
    });
}

/// Push a Discord continuation derived from an already-read playback event.
/// The exact token rejects owner -> guest -> owner cycles; track + generation
/// reject a later local edge within the same owner authority.
pub fn discord_push_observed(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    owner_token: OwnerActionToken,
    expected_track_id: u64,
) {
    let expected_generation = SCROBBLE_GEN.load(Ordering::SeqCst);
    spawn_discord_observed(
        runtime,
        owner_token,
        expected_track_id,
        expected_generation,
    );
}

/// Discord-only peer edge. It shares the playback integration generation so
/// an A -> B -> A sequence cannot revive a queued push for the first A.
pub fn discord_track_change_edge(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    owner_token: OwnerActionToken,
    expected_track_id: u64,
) {
    let expected_generation = SCROBBLE_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    spawn_discord_observed(
        runtime,
        owner_token,
        expected_track_id,
        expected_generation,
    );
}

fn spawn_discord_observed(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    owner_token: OwnerActionToken,
    expected_track_id: u64,
    expected_generation: u64,
) {
    if !discord().is_enabled() || expected_track_id == 0 {
        return;
    }
    let runtime = runtime.clone();
    crate::spawn(async move {
        let Some(_owner_action) = begin_owner_action_exact(owner_token).await else {
            return;
        };
        push_discord_current(
            &runtime,
            Some(expected_track_id),
            Some(expected_generation),
        )
        .await;
    });
}

async fn push_discord_current(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    expected_track_id: Option<u64>,
    expected_generation: Option<u64>,
) {
    if expected_generation
        .is_some_and(|expected| expected != SCROBBLE_GEN.load(Ordering::SeqCst))
    {
        return;
    }
    let state = runtime.core().get_queue_state().await;
    // The queue read yields. A newer track edge may have advanced the shared
    // generation while this task was suspended; in particular, an old empty
    // snapshot must not clear the newer track's Discord activity.
    if expected_generation
        .is_some_and(|expected| expected != SCROBBLE_GEN.load(Ordering::SeqCst))
    {
        return;
    }
    let Some(track) = state.current_track else {
        // Nothing playing — drop the activity.
        let _ = tokio::task::spawn_blocking(|| discord().clear()).await;
        return;
    };
    if !integration_snapshot_matches(
        expected_track_id,
        expected_generation,
        track.id,
        SCROBBLE_GEN.load(Ordering::SeqCst),
    ) {
        return;
    }
    let pb = runtime.core().get_playback_state();
    let title = match track.version.as_deref().filter(|v| !v.is_empty()) {
        Some(version) => format!("{} ({version})", track.title),
        None => track.title.clone(),
    };
    // Discord's large_image needs an http(s) URL or an asset key; local /
    // Plex covers are filesystem paths Discord can't fetch, so drop them
    // (the core falls back to the "cover" asset key). A sized Qobuz cover
    // is rewritten to the 600 tier first — the queue can carry the 50px
    // `small` from a restored session, and Discord renders whatever the
    // suffix says (owner smoke 2026-08-15: pixelated presence art).
    let cover_url = track
        .artwork_url
        .clone()
        .filter(|u| u.starts_with("http://") || u.starts_with("https://"))
        .map(|u| qbz_models::qobuz_cover_at_px(&u, 600).unwrap_or(u));
    let meta = NowListening {
        title,
        artist: track.artist.clone(),
        album: track.album.clone(),
        is_playing: pb.is_playing,
        current_time: pb.position as f64,
        duration: track.duration_secs as f64,
        cover_url,
    };
    let _ = tokio::task::spawn_blocking(move || discord().update(&meta)).await;
}

/// Tear down the live activity + IPC connection (logout / app exit).
/// GLUE (optional): call from the logout path and the quit handler.
pub fn discord_clear() {
    crate::spawn(async {
        let _ = tokio::task::spawn_blocking(|| discord().clear()).await;
    });
}

// ---------------------------------------------------------------------------
// Connection flows (ScrobbleActions)
// ---------------------------------------------------------------------------

/// ListenBrainz paste-token flow (scrobble.rs `listenbrainz_set_token`):
/// validate against `/validate-token`, persist the token + username to this
/// build's store AND the shared cache (so the other frontends see the same
/// sign-in), and force-enable on the first connect.
pub async fn listenbrainz_set_token(token: &str) {
    let token = token.trim().to_string();
    let start = {
        let mut g = ui_state().lock().unwrap();
        begin_listenbrainz_validation(
            &mut g,
            !token.is_empty(),
            qbz_i18n::t("Validating..."),
            qbz_i18n::t("Paste your ListenBrainz user token first"),
        )
    };
    match start {
        ListenBrainzValidationStart::Busy => return,
        ListenBrainzValidationStart::MissingToken => {
            crate::settings_qt::publish_snapshot().await;
            return;
        }
        ListenBrainzValidationStart::Started => {}
    }
    crate::settings_qt::publish_snapshot().await;

    let client = ListenBrainzClient::new();
    let (status, kind) = match client.set_token(&token).await {
        Ok(info) => {
            if let Err(e) = scrobble().set_listenbrainz_token(&token, &info.user_name) {
                log::error!("[qbz-qt] persist listenbrainz token failed: {e}");
            }
            // First-connect force-enable (scrobble.rs).
            if !scrobble_settings().listenbrainz_enabled {
                let _ = scrobble().set_listenbrainz_enabled(true);
            }
            // Write-through to the shared cache (the Slint/Tauri builds read
            // it at session start). Best-effort.
            if let Some(path) = listenbrainz_cache_path() {
                let tok = token.clone();
                let name = info.user_name.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    ListenBrainzCache::new(&path).and_then(|c| c.save_credentials(&tok, &name))
                })
                .await;
            }
            let status = qbz_i18n::t_args("Connected as {}", &[info.user_name.as_str()]);
            request_flush();
            (status, 2)
        }
        Err(e) => (qbz_i18n::t_args("Error: {}", &[&e.to_string()]), 3),
    };
    finish_listenbrainz_validation(&mut ui_state().lock().unwrap(), status, kind);
    crate::settings_qt::publish_snapshot().await;
}

/// integrations_action dispatch (non-toggle rows).
pub async fn handle_action(_runtime: &Arc<AppRuntime<LoggingAdapter>>, action: &str) {
    match action {
        // Privacy: "Clear listening history" (confirmed in QML first).
        // DELETE + VACUUM on the per-user listen_log.db; the toggle state and
        // the install's origin id survive.
        "listen-history-clear" => match crate::listen_log_qt::clear().await {
            Ok(()) => crate::toast_qt::success(qbz_i18n::t("Listening history cleared.")),
            Err(e) => {
                log::warn!("[qbz-qt] clear listening history failed: {e}");
                crate::toast_qt::error(qbz_i18n::t("Couldn't clear listening history."));
            }
        },
        // Last.fm two-step browser auth (scrobble.rs `lastfm_connect`):
        // request token -> authorize URL in the browser -> Finish exchanges
        // it for a session.
        "lastfm-connect" => {
            {
                let mut g = ui_state().lock().unwrap();
                if g.lastfm_busy {
                    return;
                }
                g.lastfm_busy = true;
            }
            crate::settings_qt::publish_snapshot().await;
            let client = LastFmClient::new();
            match client.get_token().await {
                Ok((token, url)) => {
                    {
                        let mut g = ui_state().lock().unwrap();
                        g.pending_lastfm_token = token;
                        g.lastfm_auth_url = url.clone();
                    }
                    set_status(
                        qbz_i18n::t("Authorize QBZ in your browser, then click \"Finish\""),
                        1,
                    );
                    if let Err(e) = open::that(&url) {
                        log::warn!("[qbz-qt] open Last.fm authorize page failed: {e}");
                    }
                }
                Err(e) => set_status(qbz_i18n::t_args("Error: {}", &[&e.to_string()]), 3),
            }
            ui_state().lock().unwrap().lastfm_busy = false;
            crate::settings_qt::publish_snapshot().await;
        }
        "lastfm-open-auth-url" => {
            let url = ui_state().lock().unwrap().lastfm_auth_url.clone();
            if !url.is_empty() {
                if let Err(e) = open::that(&url) {
                    log::warn!("[qbz-qt] open Last.fm authorize page failed: {e}");
                }
            }
        }
        "lastfm-finish" => {
            let token = ui_state().lock().unwrap().pending_lastfm_token.clone();
            if token.is_empty() {
                set_status(qbz_i18n::t("Start the sign-in first"), 3);
                crate::settings_qt::publish_snapshot().await;
                return;
            }
            {
                let mut g = ui_state().lock().unwrap();
                if g.lastfm_busy {
                    return;
                }
                g.lastfm_busy = true;
            }
            crate::settings_qt::publish_snapshot().await;
            let mut client = LastFmClient::new();
            match client.get_session(&token).await {
                Ok(session) => {
                    if let Err(e) = scrobble().set_lastfm_session(&session.key, &session.name) {
                        log::error!("[qbz-qt] persist lastfm session failed: {e}");
                    }
                    // First-connect force-enable (scrobble.rs).
                    if !scrobble_settings().lastfm_enabled {
                        let _ = scrobble().set_lastfm_enabled(true);
                    }
                    {
                        let mut g = ui_state().lock().unwrap();
                        g.pending_lastfm_token.clear();
                        g.lastfm_auth_url.clear();
                    }
                    set_status(
                        qbz_i18n::t_args("Connected as {}", &[session.name.as_str()]),
                        2,
                    );
                    request_flush();
                }
                Err(e) => set_status(
                    qbz_i18n::t_args(
                        "Error: {} (did you authorize in the browser?)",
                        &[&e.to_string()],
                    ),
                    3,
                ),
            }
            ui_state().lock().unwrap().lastfm_busy = false;
            crate::settings_qt::publish_snapshot().await;
        }
        "lastfm-disconnect" => {
            if let Err(e) = scrobble().disconnect_lastfm() {
                log::error!("[qbz-qt] lastfm disconnect failed: {e}");
            }
            {
                let mut g = ui_state().lock().unwrap();
                g.pending_lastfm_token.clear();
                g.lastfm_auth_url.clear();
                g.lastfm_busy = false;
            }
            set_status(qbz_i18n::t("Last.fm disconnected"), 1);
            crate::settings_qt::publish_snapshot().await;
        }
        "listenbrainz-disconnect" => {
            if let Err(e) = scrobble().disconnect_listenbrainz() {
                log::error!("[qbz-qt] listenbrainz disconnect failed: {e}");
            }
            // Clear the SHARED cache credentials too (mirrors the Slint +
            // Tauri disconnect) — otherwise the next start re-adopts them.
            if let Some(path) = listenbrainz_cache_path() {
                let _ = tokio::task::spawn_blocking(move || {
                    ListenBrainzCache::new(&path).and_then(|c| c.clear_credentials())
                })
                .await;
            }
            ui_state().lock().unwrap().listenbrainz_busy = false;
            set_status(qbz_i18n::t("ListenBrainz disconnected"), 1);
            crate::settings_qt::publish_snapshot().await;
        }
        other => log::warn!("[qbz-qt] unknown integrations action: {other}"),
    }
}

// ===========================================================================
// Fire + schedule (source-agnostic: Qobuz, local AND Plex all funnel through
// the normalized QueueTrack). scrobble.rs `on_track_changed` and below.
// ===========================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScrobbleAction {
    SendNow,
    Queue,
    Drop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ScrobblePolicy {
    action: ScrobbleAction,
    /// A live network request can race a connectivity loss. Accumulated mode
    /// decides whether that failed request is retained rather than discarded.
    queue_on_failure: bool,
}

fn decide_scrobble_policy(
    status: OfflineStatus,
    offline: OfflineModeSettings,
    qobuz_authenticated: bool,
    allow_logged_out: bool,
) -> ScrobblePolicy {
    let queue_on_failure = offline.allow_accumulated_scrobbling;
    let queued_or_dropped = if queue_on_failure {
        ScrobbleAction::Queue
    } else {
        ScrobbleAction::Drop
    };

    // The new explicit privacy gate wins over every transport policy.
    if !qobuz_authenticated && !allow_logged_out {
        return ScrobblePolicy {
            action: ScrobbleAction::Drop,
            queue_on_failure: false,
        };
    }

    let action = match status.mode {
        OfflineMode::Online => ScrobbleAction::SendNow,
        OfflineMode::InducedOffline => {
            // Manual offline closes only Qobuz's gate. The independent
            // scrobblers may still use a real network when the historical
            // immediate preference says so.
            if status.connectivity != Connectivity::Down && offline.allow_immediate_scrobbling {
                ScrobbleAction::SendNow
            } else {
                queued_or_dropped
            }
        }
        OfflineMode::RealOffline => {
            // An unauthenticated offline shell is classified RealOffline even
            // with working internet. Logged-out scrobbling is precisely the
            // exception: independent service credentials remain usable.
            if !qobuz_authenticated && status.connectivity != Connectivity::Down {
                ScrobbleAction::SendNow
            } else {
                queued_or_dropped
            }
        }
    };

    ScrobblePolicy {
        action,
        queue_on_failure,
    }
}

async fn offline_scrobble_settings() -> OfflineModeSettings {
    if let Ok(settings) = crate::offline_fwd::engine().settings() {
        return settings;
    }
    // `OfflineModeEngine::teardown` must drop its per-user store on logout,
    // while this feature deliberately keeps the last scrobbler profile. Read
    // the same DB through that retained binding for a track that continues on
    // the login screen.
    let Some(dir) = scrobble_user_dir() else {
        return OfflineModeSettings::default();
    };
    tokio::task::spawn_blocking(move || {
        OfflineModeStore::new_at(&dir).and_then(|store| store.get_settings())
    })
    .await
    .ok()
    .and_then(Result::ok)
    .unwrap_or_default()
}

async fn current_scrobble_policy(cfg: &ScrobblerSettings) -> ScrobblePolicy {
    decide_scrobble_policy(
        crate::offline_fwd::engine().status(),
        offline_scrobble_settings().await,
        QOBUZ_AUTHENTICATED.load(Ordering::SeqCst),
        cfg.allow_logged_out_scrobbling,
    )
}

async fn can_send_now(cfg: &ScrobblerSettings) -> bool {
    (cfg.lastfm_active() || cfg.listenbrainz_active())
        && current_scrobble_policy(cfg).await.action == ScrobbleAction::SendNow
}

fn request_flush() {
    crate::spawn(async { flush_if_allowed().await });
}

/// Normalized track facts the fire path needs. The title is the
/// version-enriched display title so remixes/editions scrobble correctly
/// (issue #360 parity).
#[derive(Clone)]
pub struct ScrobbleMeta {
    pub artist: String,
    pub track: String,
    /// `None` when empty — the clients take `Option<&str>` for album.
    pub album: Option<String>,
    pub duration_secs: u64,
}

/// Build the fire meta from the current queue track (keeps the glue one line).
pub fn meta_from_queue_track(track: &QueueTrack) -> ScrobbleMeta {
    let title = match track.version.as_deref().filter(|v| !v.is_empty()) {
        Some(version) => format!("{} ({version})", track.title),
        None => track.title.clone(),
    };
    ScrobbleMeta {
        artist: track.artist.clone(),
        track: title,
        // The clean album name — `album_version` is deliberately NOT appended
        // (qbz-models: Last.fm wants the clean name).
        album: Some(track.album.clone()).filter(|a| !a.is_empty()),
        duration_secs: track.duration_secs,
    }
}

/// Monotonic generation, bumped on every track change so a delayed scrobble
/// timer that fires after the user skipped is dropped (the Svelte
/// `clearTimeout` equivalent).
///
/// The wait itself is NOT wall-clock any more: it is timed by the player's
/// own position while it is playing, so a pause holds the scrobble and a
/// stop drops it (`qbzd/src/scrobble_engine.rs` — `Playing{started_at,..}`
/// + `PositionUpdated` — is the reference; the old `sleep(wait)` scrobbled a
/// track the user had paused two minutes in).
static SCROBBLE_GEN: AtomicU64 = AtomicU64::new(0);

/// How often the delayed-scrobble task samples the player while waiting.
const SCROBBLE_TICK: Duration = Duration::from_secs(1);

/// Cancel every owner-playback integration at the authority commit boundary.
/// Exact tokens already reject work after the install; the generation wakes
/// delayed same-owner timers promptly, and clearing Discord removes the
/// owner's presence before delegated playback becomes observable.
pub fn cancel_owner_playback_tasks() {
    let cancelled_generation = SCROBBLE_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    crate::spawn(async move {
        if SCROBBLE_GEN.load(Ordering::SeqCst) != cancelled_generation {
            return;
        }
        let _ = tokio::task::spawn_blocking(|| discord().clear()).await;
    });
}

/// Shared track/generation validation for queued integration continuations.
/// `None` means the task obtained its own exact owner lease before reading the
/// queue and did not derive its input from an earlier playback snapshot.
fn integration_snapshot_matches(
    expected_track_id: Option<u64>,
    expected_generation: Option<u64>,
    observed_track_id: u64,
    observed_generation: u64,
) -> bool {
    expected_track_id.is_none_or(|expected| expected == observed_track_id)
        && expected_generation.is_none_or(|expected| expected == observed_generation)
}

/// Fires now-playing immediately, then waits for the exact owner's track to
/// reach `min(50% of duration, 240s)`. No lease is held during the long wait;
/// each observation is re-admitted with the token captured before the queue
/// snapshot, and the irreversible send keeps that exact permit alive.
fn spawn_scrobble(
    meta: ScrobbleMeta,
    cfg: ScrobblerSettings,
    owner_token: OwnerActionToken,
    expected_track_id: u64,
    expected_generation: u64,
) {
    // Last.fm wants the time the track STARTED, captured here on the edge —
    // never the time the threshold fired (a 240 s wait on a long track put
    // every scrobble four minutes late).
    let started_at = unix_now();
    crate::spawn(async move {
        let rt = crate::app();
        let Some(initial_owner_action) = begin_owner_action_exact(owner_token).await else {
            return;
        };
        let initial_event = rt.core().player().get_playback_event();
        if !integration_snapshot_matches(
            Some(expected_track_id),
            Some(expected_generation),
            initial_event.track_id,
            SCROBBLE_GEN.load(Ordering::SeqCst),
        ) {
            return;
        }
        // Now-playing is never queued. It may use the independent network in
        // manual-offline immediate mode or an opted-in logged-out session.
        if current_scrobble_policy(&cfg).await.action == ScrobbleAction::SendNow {
            let current_event = rt.core().player().get_playback_event();
            if integration_snapshot_matches(
                Some(expected_track_id),
                Some(expected_generation),
                current_event.track_id,
                SCROBBLE_GEN.load(Ordering::SeqCst),
            ) {
                send_now_playing(&meta, &cfg).await;
            }
        }
        drop(initial_owner_action);

        // Delayed scrobble. Unknown / too-short duration: skip (Last.fm's
        // "longer than 30 seconds" rule lives in qbz_app::scrobble_timing).
        let Some(wait) = scrobble_delay_secs(meta.duration_secs) else {
            log::debug!(
                "[qbz-qt] scrobble: skip delayed scrobble, unusable duration for '{}'",
                meta.track
            );
            return;
        };
        // Wait for the PLAYER to reach the threshold, not the clock: sample
        // its position once a second and only count it while playing. A
        // pause simply keeps waiting; a stop (no track loaded) ends the wait
        // without a scrobble; a newer track edge self-cancels via the
        // generation, as before. Seeks move the position and therefore the
        // moment this fires — the same rule the daemon applies.
        let mut ticker = tokio::time::interval(SCROBBLE_TICK);
        loop {
            ticker.tick().await;
            let Some(threshold_owner_action) = begin_owner_action_exact(owner_token).await else {
                return;
            };
            let ev = rt.core().player().get_playback_event();
            if !integration_snapshot_matches(
                Some(expected_track_id),
                Some(expected_generation),
                ev.track_id,
                SCROBBLE_GEN.load(Ordering::SeqCst),
            ) {
                log::debug!(
                    "[qbz-qt] scrobble: owner track changed before the threshold, dropping '{}'",
                    meta.track
                );
                return;
            }
            if ev.is_playing && ev.position >= wait {
                // Keep the exact permit through policy revalidation and the
                // irreversible external send/queue operation.
                send_scrobble(&meta, started_at).await;
                drop(threshold_owner_action);
                return;
            }
            drop(threshold_owner_action);
        }
    });
}

/// The whole integrations reaction to a LOCAL track-change edge, in one call:
/// scrobble now-playing + arm the delayed scrobble, and refresh the Discord
/// presence. Both halves early-return when the user has not opted in, so this
/// is free for everyone else (the queue state is only read when at least one
/// integration is live).
///
/// §11.2 SPLIT (Slint playback.rs:2266-2278): the peer-active guard covers
/// the SCROBBLE half only — never scrobble a peer's audio. The Discord half
/// is deliberately UNGUARDED: in Slint controller mode Discord keeps
/// following the peer's tracks while scrobbles are suppressed (the push
/// inside `refresh_now_playing_meta`, playback.rs:2236-2239, which the peer
/// track edge also reaches, :5137-5141). The poll loop's PEER edge therefore
/// calls [`discord_push_observed`] directly and never this entry. The ephemeral-mode
/// product law (scrobble MAY happen) is unaffected — this guard is about
/// REMOTE ownership, not ephemeral.
///
/// GLUE: call from the playback poll's DE-DUPED track-change edge
/// (`track_id != last_track_id`), never from `refresh_now_playing` — that runs
/// on every play/queue republish and would re-arm the scrobble timer.
pub fn on_track_change_edge(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    owner_token: OwnerActionToken,
    expected_track_id: u64,
) {
    // Bump synchronously at the producer edge. If two spawned queue reads run
    // out of order, the older one cannot make itself newest after the fact.
    let expected_generation = SCROBBLE_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    let cfg = scrobble_settings();
    let scrobblers_live = cfg.lastfm_active() || cfg.listenbrainz_active();
    if !scrobblers_live && !discord().is_enabled() {
        return;
    }
    let rt = runtime.clone();
    crate::spawn(async move {
        let Some(_owner_action) = begin_owner_action_exact(owner_token).await else {
            return;
        };
        let state = rt.core().get_queue_state().await;
        let Some(track) = state.current_track else {
            return;
        };
        if !integration_snapshot_matches(
            Some(expected_track_id),
            Some(expected_generation),
            track.id,
            SCROBBLE_GEN.load(Ordering::SeqCst),
        ) {
            return;
        }
        if scrobblers_live {
            // The guard (playback.rs:2267-2271): a remote QConnect renderer
            // owns playback — skip the scrobble half ONLY. The spawn's other
            // half (Discord) must still run, so this is a skip, not the
            // reference's early `return` (its spawn holds no Discord half).
            // Fires in the topology-vs-snapshot window, where `is_peer_active`
            // is already true but the poll loop still runs the LOCAL path.
            let peer_active = match crate::qconnect_qt::service() {
                Some(svc) => svc.is_peer_active().await,
                None => false,
            };
            if !peer_active {
                spawn_scrobble(
                    meta_from_queue_track(&track),
                    cfg,
                    owner_token,
                    expected_track_id,
                    expected_generation,
                );
            }
        }
        // UNGUARDED, 1:1 with the reference — Discord follows the peer.
        if discord().is_enabled() {
            push_discord_current(
                &rt,
                Some(expected_track_id),
                Some(expected_generation),
            )
            .await;
        }
    });
}

/// Optional ListenBrainz extras — duration is the only one the QueueTrack
/// carries (no ISRC / MB IDs on the queue model yet).
fn lb_info(duration_secs: u64) -> Option<AdditionalInfo> {
    Some(AdditionalInfo {
        duration_ms: (duration_secs > 0).then_some(duration_secs * 1000),
        ..Default::default()
    })
}

/// Fire "now playing" for each enabled service. Failures only log — the
/// scrobble path is what queues.
async fn send_now_playing(meta: &ScrobbleMeta, cfg: &ScrobblerSettings) {
    let album = meta.album.as_deref();
    if cfg.lastfm_active() {
        let client = LastFmClient::with_session_key(cfg.lastfm_session_key.clone());
        if let Err(e) = client
            .update_now_playing(&meta.artist, &meta.track, album)
            .await
        {
            log::debug!("[qbz-qt] Last.fm now-playing failed: {e}");
        }
    }
    if cfg.listenbrainz_active() {
        let client = ListenBrainzClient::new();
        client
            .restore_token(
                cfg.listenbrainz_token.clone(),
                cfg.listenbrainz_username.clone(),
            )
            .await;
        if let Err(e) = client
            .submit_playing_now(
                &meta.artist,
                &meta.track,
                album,
                lb_info(meta.duration_secs),
            )
            .await
        {
            log::debug!("[qbz-qt] ListenBrainz now-playing failed: {e}");
        }
    }
}

/// Fire, retain or drop the actual scrobble according to the restored offline
/// policy plus the logged-out opt-out. Settings are re-read in case the user
/// changed a policy or disconnected while the timer waited.
///
/// `started_at` is the unix time the track STARTED (captured on the track
/// edge), which is what both services define as the listen's timestamp.
async fn send_scrobble(meta: &ScrobbleMeta, started_at: i64) {
    let cfg = scrobble_settings();
    let album = meta.album.as_deref();
    let timestamp = started_at;
    let policy = current_scrobble_policy(&cfg).await;

    if cfg.lastfm_active() {
        match policy.action {
            ScrobbleAction::SendNow => {
                let client = LastFmClient::with_session_key(cfg.lastfm_session_key.clone());
                match client
                    .scrobble(&meta.artist, &meta.track, album, timestamp as u64)
                    .await
                {
                    Ok(()) => log::info!(
                        "[qbz-qt] Last.fm scrobbled: {} - {}",
                        meta.artist,
                        meta.track
                    ),
                    Err(e) if policy.queue_on_failure => {
                        log::warn!("[qbz-qt] Last.fm scrobble failed ({e}); queueing for later");
                        queue_lastfm(meta, timestamp).await;
                    }
                    Err(e) => log::warn!(
                        "[qbz-qt] Last.fm scrobble failed ({e}); accumulation disabled, dropping"
                    ),
                }
            }
            ScrobbleAction::Queue => queue_lastfm(meta, timestamp).await,
            ScrobbleAction::Drop => {
                log::debug!("[qbz-qt] Last.fm scrobble dropped by offline/logout policy")
            }
        }
    }

    if cfg.listenbrainz_active() {
        match policy.action {
            ScrobbleAction::SendNow => {
                let client = ListenBrainzClient::new();
                client
                    .restore_token(
                        cfg.listenbrainz_token.clone(),
                        cfg.listenbrainz_username.clone(),
                    )
                    .await;
                match client
                    .submit_listen(
                        &meta.artist,
                        &meta.track,
                        album,
                        timestamp,
                        lb_info(meta.duration_secs),
                    )
                    .await
                {
                    Ok(()) => log::info!(
                        "[qbz-qt] ListenBrainz scrobbled: {} - {}",
                        meta.artist,
                        meta.track
                    ),
                    Err(e) if policy.queue_on_failure => {
                        log::warn!(
                            "[qbz-qt] ListenBrainz scrobble failed ({e}); queueing for later"
                        );
                        queue_listenbrainz(meta, timestamp).await;
                    }
                    Err(e) => log::warn!(
                        "[qbz-qt] ListenBrainz scrobble failed ({e}); accumulation disabled, dropping"
                    ),
                }
            }
            ScrobbleAction::Queue => queue_listenbrainz(meta, timestamp).await,
            ScrobbleAction::Drop => {
                log::debug!("[qbz-qt] ListenBrainz scrobble dropped by offline/logout policy")
            }
        }
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Queue a Last.fm scrobble into the SHARED per-user `offline_settings.db`
/// `scrobble_queue` (the table the other frontends queue into and flush from).
async fn queue_lastfm(meta: &ScrobbleMeta, timestamp: i64) {
    let Some(dir) = scrobble_user_dir() else {
        return;
    };
    let artist = meta.artist.clone();
    let track = meta.track.clone();
    let album = meta.album.clone();
    let _ = tokio::task::spawn_blocking(move || match OfflineModeStore::new_at(&dir) {
        Ok(store) => {
            if let Err(e) = store.queue_scrobble(&artist, &track, album.as_deref(), timestamp) {
                log::warn!("[qbz-qt] queue Last.fm scrobble failed: {e}");
            }
        }
        Err(e) => log::warn!("[qbz-qt] open offline settings store failed: {e}"),
    })
    .await;
}

/// Queue a ListenBrainz listen into the SHARED per-user
/// `ListenBrainzCache.listen_queue` (the canonical LB offline store).
async fn queue_listenbrainz(meta: &ScrobbleMeta, timestamp: i64) {
    let Some(path) = listenbrainz_cache_path() else {
        return;
    };
    let artist = meta.artist.clone();
    let track = meta.track.clone();
    let album = meta.album.clone();
    let duration_ms = (meta.duration_secs > 0).then_some(meta.duration_secs * 1000);
    let _ = tokio::task::spawn_blocking(move || match ListenBrainzCache::new(&path) {
        Ok(cache) => {
            if let Err(e) = cache.queue_listen(
                timestamp,
                &artist,
                &track,
                album.as_deref(),
                None,
                None,
                None,
                None,
                duration_ms,
            ) {
                log::warn!("[qbz-qt] queue ListenBrainz listen failed: {e}");
            }
        }
        Err(e) => log::warn!("[qbz-qt] open ListenBrainz cache failed: {e}"),
    })
    .await;
}

// ---------------------------------------------------------------------------
// Offline flush — drain both queues whenever policy becomes sendable
// ---------------------------------------------------------------------------

async fn flush_if_allowed() {
    let cfg = scrobble_settings();
    if can_send_now(&cfg).await {
        flush_offline_queues().await;
    }
}

async fn flush_offline_queues() {
    flush_lastfm_queue().await;
    flush_listenbrainz_queue().await;
}

/// Flush the Last.fm queue: up to 50 per pass (the Last.fm batch limit),
/// oldest first; entries older than 14 days are dropped (marked sent) since
/// Last.fm rejects them. Stops at the first network failure and retries on the
/// next edge. Cleans up sent rows older than 7 days afterwards.
async fn flush_lastfm_queue() {
    let cfg = scrobble_settings();
    if !cfg.lastfm_active() {
        return;
    }
    let Some(dir) = scrobble_user_dir() else {
        return;
    };
    let pending = match tokio::task::spawn_blocking({
        let dir = dir.clone();
        move || OfflineModeStore::new_at(&dir).and_then(|s| s.get_queued_scrobbles(50))
    })
    .await
    {
        Ok(Ok(p)) => p,
        _ => return,
    };
    if pending.is_empty() {
        return;
    }

    let client = LastFmClient::with_session_key(cfg.lastfm_session_key.clone());
    let cutoff = unix_now() - 14 * 86400;
    let mut sent_ids: Vec<i64> = Vec::new();
    for item in pending {
        if item.timestamp < cutoff {
            // Too old for Last.fm — drop it (mark sent so it stops re-trying).
            sent_ids.push(item.id);
            continue;
        }
        match client
            .scrobble(
                &item.artist,
                &item.track,
                item.album.as_deref(),
                item.timestamp as u64,
            )
            .await
        {
            Ok(()) => sent_ids.push(item.id),
            Err(e) => {
                log::warn!(
                    "[qbz-qt] Last.fm flush stopped at {} - {}: {e}",
                    item.artist,
                    item.track
                );
                break; // still offline / failing — retry on the next edge
            }
        }
    }
    if !sent_ids.is_empty() {
        let count = sent_ids.len();
        let _ = tokio::task::spawn_blocking(move || {
            OfflineModeStore::new_at(&dir).and_then(|s| {
                s.mark_scrobbles_sent(&sent_ids)?;
                s.cleanup_sent_scrobbles(7)
            })
        })
        .await;
        log::info!("[qbz-qt] Last.fm flush: {count} scrobble(s) sent/cleared");
    }
}

/// Flush the ListenBrainz queue from the shared cache. Stops at the first
/// failure and retries on the next edge.
async fn flush_listenbrainz_queue() {
    let cfg = scrobble_settings();
    if !cfg.listenbrainz_active() {
        return;
    }
    let Some(path) = listenbrainz_cache_path() else {
        return;
    };
    let pending = match tokio::task::spawn_blocking({
        let path = path.clone();
        move || ListenBrainzCache::new(&path).and_then(|c| c.get_pending_listens(500))
    })
    .await
    {
        Ok(Ok(p)) => p,
        _ => return,
    };
    if pending.is_empty() {
        return;
    }

    let client = ListenBrainzClient::new();
    client
        .restore_token(
            cfg.listenbrainz_token.clone(),
            cfg.listenbrainz_username.clone(),
        )
        .await;
    let mut sent_ids: Vec<i64> = Vec::new();
    for item in pending {
        let info = AdditionalInfo {
            recording_mbid: item.recording_mbid.clone(),
            release_mbid: item.release_mbid.clone(),
            artist_mbids: item.artist_mbids.clone(),
            isrc: item.isrc.clone(),
            duration_ms: item.duration_ms,
            ..Default::default()
        };
        if client
            .submit_listen(
                &item.artist_name,
                &item.track_name,
                item.release_name.as_deref(),
                item.listened_at,
                Some(info),
            )
            .await
            .is_ok()
        {
            sent_ids.push(item.id);
        } else {
            break; // still failing — retry on the next edge
        }
    }
    if !sent_ids.is_empty() {
        let count = sent_ids.len();
        let _ = tokio::task::spawn_blocking(move || {
            ListenBrainzCache::new(&path).and_then(|c| c.mark_listens_sent(&sent_ids))
        })
        .await;
        log::info!("[qbz-qt] ListenBrainz flush: {count} listen(s) sent");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

    fn unique_store_dir(name: &str) -> std::path::PathBuf {
        static NONCE: AtomicU64 = AtomicU64::new(0);
        let nonce = NONCE.fetch_add(1, AtomicOrdering::Relaxed);
        std::env::temp_dir().join(format!(
            "qbz-qt-integrations-{name}-{}-{nonce}",
            std::process::id()
        ))
    }

    fn status(
        mode: OfflineMode,
        connectivity: Connectivity,
        offline_session: bool,
    ) -> OfflineStatus {
        OfflineStatus {
            mode,
            connectivity,
            captive_portal: false,
            induced: mode == OfflineMode::InducedOffline,
            offline_session,
        }
    }

    #[test]
    fn listenbrainz_validation_reports_progress_and_ignores_reentrant_empty_submit() {
        let mut ui = IntegrationUi::default();
        assert_eq!(
            begin_listenbrainz_validation(
                &mut ui,
                true,
                "Validating...".into(),
                "Missing token".into(),
            ),
            ListenBrainzValidationStart::Started
        );
        assert!(ui.listenbrainz_busy);
        assert_eq!(ui.status_text, "Validating...");
        assert_eq!(ui.status_kind, 1);

        // Regression: blur submitted the real token, then the button's broken
        // child lookup submitted "" and painted a false error over progress.
        assert_eq!(
            begin_listenbrainz_validation(
                &mut ui,
                false,
                "Validating again...".into(),
                "Missing token".into(),
            ),
            ListenBrainzValidationStart::Busy
        );
        assert!(ui.listenbrainz_busy);
        assert_eq!(ui.status_text, "Validating...");
        assert_eq!(ui.status_kind, 1);

        finish_listenbrainz_validation(&mut ui, "Connected as listener".into(), 2);
        assert!(!ui.listenbrainz_busy);
        assert_eq!(ui.status_text, "Connected as listener");
        assert_eq!(ui.status_kind, 2);
    }

    #[test]
    fn listenbrainz_missing_token_is_an_idle_error() {
        let mut ui = IntegrationUi::default();
        assert_eq!(
            begin_listenbrainz_validation(
                &mut ui,
                false,
                "Validating...".into(),
                "Missing token".into(),
            ),
            ListenBrainzValidationStart::MissingToken
        );
        assert!(!ui.listenbrainz_busy);
        assert_eq!(ui.status_text, "Missing token");
        assert_eq!(ui.status_kind, 3);
    }

    #[test]
    fn manual_offline_respects_immediate_and_accumulated_modes() {
        let manual = status(OfflineMode::InducedOffline, Connectivity::Up, false);
        let accumulated = OfflineModeSettings::default();
        assert_eq!(
            decide_scrobble_policy(manual, accumulated, true, true).action,
            ScrobbleAction::Queue
        );

        let immediate = OfflineModeSettings {
            allow_immediate_scrobbling: true,
            allow_accumulated_scrobbling: false,
            ..OfflineModeSettings::default()
        };
        assert_eq!(
            decide_scrobble_policy(manual, immediate, true, true).action,
            ScrobbleAction::SendNow
        );
    }

    #[test]
    fn physical_offline_only_retains_when_accumulation_is_enabled() {
        let physical = status(OfflineMode::RealOffline, Connectivity::Down, false);
        assert_eq!(
            decide_scrobble_policy(physical, OfflineModeSettings::default(), true, true).action,
            ScrobbleAction::Queue
        );

        let disabled = OfflineModeSettings {
            allow_accumulated_scrobbling: false,
            ..OfflineModeSettings::default()
        };
        assert_eq!(
            decide_scrobble_policy(physical, disabled, true, true).action,
            ScrobbleAction::Drop
        );
    }

    #[test]
    fn logged_out_gate_allows_independent_network_but_can_opt_out() {
        // A regular logout tears the offline engine down, so it reports
        // Online while the independent Qobuz-auth flag is false.
        let login_screen = status(OfflineMode::Online, Connectivity::Up, false);
        // Starting the unauthenticated offline shell keeps the explicit
        // offline-session classification even when the network is usable.
        let offline_shell = status(OfflineMode::RealOffline, Connectivity::Up, true);

        for logged_out in [login_screen, offline_shell] {
            assert_eq!(
                decide_scrobble_policy(logged_out, OfflineModeSettings::default(), false, true)
                    .action,
                ScrobbleAction::SendNow
            );
            assert_eq!(
                decide_scrobble_policy(logged_out, OfflineModeSettings::default(), false, false)
                    .action,
                ScrobbleAction::Drop
            );
        }
    }

    #[test]
    fn live_send_failure_only_queues_in_accumulated_mode() {
        let online = status(OfflineMode::Online, Connectivity::Up, false);
        let accumulated =
            decide_scrobble_policy(online, OfflineModeSettings::default(), true, true);
        assert_eq!(accumulated.action, ScrobbleAction::SendNow);
        assert!(accumulated.queue_on_failure);

        let no_accumulation = decide_scrobble_policy(
            online,
            OfflineModeSettings {
                allow_accumulated_scrobbling: false,
                ..OfflineModeSettings::default()
            },
            true,
            true,
        );
        assert_eq!(no_accumulation.action, ScrobbleAction::SendNow);
        assert!(!no_accumulation.queue_on_failure);
    }

    #[test]
    fn queued_integration_requires_the_exact_track_and_generation() {
        assert!(integration_snapshot_matches(Some(41), Some(7), 41, 7));
        assert!(!integration_snapshot_matches(Some(41), Some(7), 42, 7));
        assert!(!integration_snapshot_matches(Some(41), Some(7), 41, 8));

        // A -> B -> A is still stale: the numeric track matches again, but the
        // producer generation cannot be revived by the later A edge.
        assert!(!integration_snapshot_matches(Some(41), Some(7), 41, 9));
    }

    #[test]
    fn task_owned_snapshot_needs_no_prior_track_identity() {
        assert!(integration_snapshot_matches(None, None, 99, 12));
    }

    #[test]
    fn discover_preferences_rebind_between_guest_and_account_profiles() {
        let guest = unique_store_dir("guest");
        let account = unique_store_dir("account");

        init_for_user(&guest);
        set_show_recommendations(false).expect("persist guest preference");
        assert!(!show_recommendations());

        init_for_user(&account);
        assert!(
            show_recommendations(),
            "account must not inherit guest prefs"
        );

        unbind_qobuz_user();
        assert!(show_recommendations(), "unbound state uses safe defaults");

        init_for_user(&guest);
        assert!(!show_recommendations(), "guest preference survives logout");

        unbind_qobuz_user();
        scrobble().teardown().expect("release scrobbler test store");
        *SCROBBLE_DIR.lock().expect("scrobbler dir lock") = None;
        let _ = std::fs::remove_dir_all(&guest);
        let _ = std::fs::remove_dir_all(&account);
    }
}
