//! Daemon host for the shared QBZ API. Playback, queue, settings and authority
//! remain here; HTTP access control, serving and catalog handlers are shared.
pub mod play;
pub mod playback;
pub mod queue;
pub mod radio;
pub mod settings;
pub mod status;

use std::io::Cursor;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Instant;
use tiny_http::{Request, Response};
use tokio::sync::broadcast;
use crate::adapter::DaemonAdapter;
use crate::paths::ProfileRoots;
use crate::state::DaemonShared;
use qbz_app::shell::AppRuntime;
use qbz_audio::settings::AudioSettingsStore;
use qbz_models::CoreEvent;
pub use qbz_control::{bind, serve, BindError};
pub(crate) use qbz_control::{canon_volume, err_json, error_body, json};
use qbz_control::{artwork, browse, discover, fav, lyrics, playlist, reco, search};

/// Everything the route handlers read. Owned by the single serving thread
/// (moved into it by [`serve`]), so it only needs `Send`, never `Sync` — which
/// is why a plain `AudioSettingsStore` (rusqlite `Connection`: Send, not Sync)
/// can live here directly. `token` is the opt-in `[server] token`, read once at
/// boot (`None` = open).
pub struct ApiState {
    /// Absent unless run --orbit. Uses daemon roots and this server's access gate.
    pub library: Option<qbz_control::library::LibraryEndpoint>,
    pub runtime: Arc<AppRuntime<DaemonAdapter>>,
    pub shared: Arc<Mutex<DaemonShared>>,
    /// The CoreEvent bus (DaemonAdapter sender). `/api/events` subscribes a
    /// receiver per SSE connection; no other route touches it.
    pub bus: broadcast::Sender<CoreEvent>,
    pub roots: ProfileRoots,
    pub token: Option<String>,
    /// The bound address, echoed verbatim by `/api/info`.
    pub bind: String,
    /// Handle to the daemon's tokio runtime — the serving thread is a plain
    /// `std::thread`, so async core calls (`get_queue_state`) run via
    /// `Handle::block_on` (never called from a runtime worker → no panic).
    pub rt: tokio::runtime::Handle,
    /// Second read-only connection to the daemon-root audio settings DB (WAL
    /// allows it alongside the Player's). Supplies `configured_device`/`backend`.
    pub audio: AudioSettingsStore,
    /// Cached device enumeration for `device_present` (refreshed on a TTL so a
    /// `status` poll never re-enumerates CPAL on every call).
    pub devices: Mutex<DeviceCache>,
    /// T11: the `AudioSettings` last applied to the `Player`, so
    /// `POST /api/settings/reload` can tell whether a routing-critical field
    /// changed since the previous reload (`daemon::audio_routing_changed`) —
    /// reinit only when it did, never on every unrelated nudge.
    pub audio_snapshot: Mutex<qbz_audio::settings::AudioSettings>,
    /// T11: the live cell the playback driver's background auto-advance reads
    /// for streaming quality (`daemon.rs::run`'s `quality_cell`) — reload
    /// writes a fresh value here after re-reading `daemon_prefs`.
    pub quality: Arc<Mutex<qbz_models::Quality>>,
    /// T11: reaches the running QConnect service (`connect`/`disconnect`/
    /// device-name refresh) once `qconnect::start` (boot step 12, AFTER the
    /// API starts serving at step 11) publishes it — empty only in the brief
    /// window between the two, which the reload handler no-ops through.
    pub qconnect_control: Arc<std::sync::OnceLock<crate::qconnect::QconnectControl>>,
}

/// TTL-cached output-device names for the `device_present` check.
#[derive(Default)]
pub struct DeviceCache {
    pub at: Option<Instant>,
    pub names: Vec<String>,
}

impl qbz_control::HttpHost for ApiState {
    fn token(&self) -> Option<&str> { self.token.as_deref() }
    fn events(&self) -> &broadcast::Sender<CoreEvent> { &self.bus }
    fn route(&self, request: &mut Request) -> Response<Cursor<Vec<u8>>> {
        route(self, request)
    }
}

/// Best-effort occupant probe for the step-5 diagnosis: `GET /api/ping` and
/// check the response identifies as qbzd (`"app":"qbzd"`). Loopback, short
/// timeout, no dependency on reqwest (the CLI's async client is not built here).
pub fn probe_is_qbzd(addr: SocketAddr) -> bool {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::Duration;
    let mut stream = match TcpStream::connect_timeout(&addr, Duration::from_millis(500)) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(1)));
    let req = format!(
        "GET /api/ping HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\n\r\n"
    );
    if stream.write_all(req.as_bytes()).is_err() {
        return false;
    }
    let _ = stream.flush();
    let mut buf = Vec::new();
    let _ = stream.take(4096).read_to_end(&mut buf);
    let text = String::from_utf8_lossy(&buf);
    text.contains("\"app\":\"qbzd\"")
}

fn route(state: &ApiState, req: &mut Request) -> Response<Cursor<Vec<u8>>> {
    let method = req.method().as_str().to_owned();
    let url = req.url().to_owned();
    let mut url_parts = url.splitn(2, '?');
    let path = url_parts.next().unwrap_or("").to_owned();
    let query = url_parts.next().unwrap_or("").to_owned();
    if path == "/api/orbit/library" || path.starts_with("/api/orbit/library/") {
        return match &state.library {
            Some(library) => library.route(req),
            None => err_json(404, "not_supported", "Orbit library is not enabled", "start the host with run --orbit"),
        };
    }
    let catalog = qbz_control::CatalogContext {
        core: state.runtime.core(),
        rt: &state.rt,
        needs_auth: state.shared.lock().map(|s| s.auth == crate::state::AuthState::NeedsAuth).unwrap_or(false),
    };

    match (method.as_str(), path.as_str()) {
        ("GET", "/api/ping") => json(
            200,
            serde_json::json!({"ok": true, "app": "qbzd", "api_version": crate::API_VERSION}),
        ),
        ("GET", "/api/info") => status::info(state),
        ("GET", "/api/status") => status::status(state),
        ("GET", "/api/now-playing") => playback::now_playing(state),
        ("POST", "/api/playback/play") => playback::play(state),
        ("POST", "/api/playback/pause") => playback::pause(state),
        ("POST", "/api/playback/toggle") => playback::toggle(state),
        ("POST", "/api/playback/stop") => playback::stop(state),
        ("POST", "/api/playback/next") => playback::next(state),
        ("POST", "/api/playback/previous") => playback::previous(state),
        ("POST", "/api/playback/seek") => {
            let body = read_json_body(req);
            playback::seek(state, &body)
        }
        ("POST", "/api/playback/volume") => {
            let body = read_json_body(req);
            playback::volume(state, &body)
        }
        ("POST", "/api/playback/shuffle") => {
            let body = read_json_body(req);
            playback::shuffle(state, &body)
        }
        ("POST", "/api/playback/repeat") => {
            let body = read_json_body(req);
            playback::repeat(state, &body)
        }
        ("GET", "/api/search") => search::search(&catalog, &query),
        ("POST", "/api/play") => {
            let body = read_json_body(req);
            play::play(state, &body)
        }
        ("GET", "/api/album") => browse::album(&catalog, &query),
        ("GET", "/api/artist") => browse::artist(&catalog, &query),
        ("GET", "/api/similar") => browse::similar(&catalog, &query),
        ("GET", "/api/suggest") => browse::suggest(&catalog, &query),
        ("GET", "/api/discover") => discover::discover(&catalog, &query),
        ("GET", "/api/lyrics") => lyrics::lyrics(&catalog, &query),
        ("GET", "/api/artwork/current") => artwork::current(&catalog),
        ("POST", "/api/radio") => {
            let body = read_json_body(req);
            radio::radio(state, &body)
        }
        ("POST", "/api/reco/playlist") => {
            let body = read_json_body(req);
            reco::playlist(&catalog, &body)
        }
        ("GET", "/api/favorites") => fav::list(&catalog, &query),
        ("POST", "/api/favorites/add") => {
            let body = read_json_body(req);
            fav::add(&catalog, &body)
        }
        ("POST", "/api/favorites/remove") => {
            let body = read_json_body(req);
            fav::remove(&catalog, &body)
        }
        ("GET", "/api/playlists") => playlist::list(&catalog),
        ("GET", "/api/playlist") => playlist::show(&catalog, &query),
        ("POST", "/api/playlist/create") => {
            let body = read_json_body(req);
            playlist::create(&catalog, &body)
        }
        ("POST", "/api/playlist/update") => {
            let body = read_json_body(req);
            playlist::update(&catalog, &body)
        }
        ("POST", "/api/playlist/delete") => {
            let body = read_json_body(req);
            playlist::delete(&catalog, &body)
        }
        ("POST", "/api/playlist/tracks/add") => {
            let body = read_json_body(req);
            playlist::tracks_add(&catalog, &body)
        }
        ("POST", "/api/playlist/tracks/remove") => {
            let body = read_json_body(req);
            playlist::tracks_remove(&catalog, &body)
        }
        ("GET", "/api/queue") => queue::list(state, &query),
        ("POST", "/api/queue/add") => {
            let body = read_json_body(req);
            queue::add(state, &body)
        }
        ("POST", "/api/queue/remove") => {
            let body = read_json_body(req);
            queue::remove(state, &body)
        }
        ("POST", "/api/queue/clear") => {
            let body = read_json_body(req);
            queue::clear(state, &body)
        }
        ("POST", "/api/queue/move") => {
            let body = read_json_body(req);
            queue::reorder(state, &body)
        }
        ("POST", "/api/queue/jump") => {
            let body = read_json_body(req);
            queue::jump(state, &body)
        }
        ("POST", "/api/queue/stop-after") => {
            let body = read_json_body(req);
            queue::stop_after(state, &body)
        }
        ("POST", "/api/settings/reload") => settings::reload(state),
        _ => err_json(404, "not_found", "unknown route", "see qbzd --help"),
    }
}

/// Read and parse a request body as JSON (T7's seek/volume POST bodies).
/// An unreadable or absent body parses to `Value::Null` — the route handlers
/// treat a missing expected field as `400 bad_request`, never a panic.
fn read_json_body(req: &mut Request) -> serde_json::Value {
    let mut buf = String::new();
    let _ = req.as_reader().read_to_string(&mut buf);
    serde_json::from_str(&buf).unwrap_or(serde_json::Value::Null)
}

/// Refuse owner-only queue and track-selection actions while the QConnect
/// authority cell belongs to a delegated runtime (or is fenced during an
/// authority transition). Transport controls that are safe for either origin
/// -- pause/resume/seek/volume -- deliberately do not call this OWNER gate;
/// they use [`transport_action_lease`] so they still drain before a handoff.
pub(crate) fn owner_action_gate(state: &ApiState) -> Option<Response<Cursor<Vec<u8>>>> {
    owner_actions_blocked(state.qconnect_control.as_ref())
        .then(|| json(409, owner_action_conflict_body()))
}

/// Atomically admit one owner-only action. The returned permit must stay alive
/// through every queue/playback mutation and await in that action; authority
/// handoff closes new admission and drains these permits before snapshotting.
/// `None` inside `Ok` is the short boot window before QConnect publishes its
/// control handle, when no delegated runtime can exist yet.
pub(crate) fn owner_action_lease(
    state: &ApiState,
) -> Result<Option<crate::qconnect::authority::AuthorityActionPermit>, Response<Cursor<Vec<u8>>>> {
    let Some(qconnect) = state.qconnect_control.get() else {
        return Ok(None);
    };
    qconnect
        .try_owner_action_permit()
        .map(Some)
        .ok_or_else(|| json(409, owner_action_conflict_body()))
}

/// Atomically admit an origin-agnostic transport action. A stable owner or
/// delegated renderer may be controlled locally, but a transition fence
/// refuses the action so it cannot land on the runtime installed after the
/// handoff. As with owner leases, `None` is only the pre-QConnect boot window.
pub(crate) fn transport_action_lease(
    state: &ApiState,
) -> Result<Option<crate::qconnect::authority::AuthorityActionPermit>, Response<Cursor<Vec<u8>>>> {
    let Some(qconnect) = state.qconnect_control.get() else {
        return Ok(None);
    };
    qconnect
        .try_transport_action_permit()
        .map(Some)
        .ok_or_else(|| json(409, transport_action_conflict_body()))
}

/// Cheap form used by spawn-and-ack handlers immediately before their async
/// owner action starts. An unpublished control means QConnect has not reached
/// daemon boot step 12 yet, so there is no delegated authority to conflict
/// with.
pub(crate) fn owner_actions_blocked(control: &OnceLock<crate::qconnect::QconnectControl>) -> bool {
    control
        .get()
        .is_some_and(|qconnect| !qconnect.owner_actions_allowed())
}

fn owner_action_conflict_body() -> serde_json::Value {
    error_body(
        "qconnect_authority_conflict",
        "the owner queue is unavailable while QConnect controls playback",
        "return playback control to the QBZ owner and try again",
    )
}

fn transport_action_conflict_body() -> serde_json::Value {
    error_body(
        "qconnect_authority_transition",
        "playback control is temporarily unavailable during a QConnect handoff",
        "retry the transport command",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn owner_action_conflict_is_stable_and_contains_no_handoff_details() {
        let body = owner_action_conflict_body();
        assert_eq!(body["error"]["code"], "qconnect_authority_conflict");
        assert_eq!(
            body["error"]["message"],
            "the owner queue is unavailable while QConnect controls playback"
        );

        let encoded = serde_json::to_string(&body).unwrap();
        for secret_shape in [
            "Authorization",
            "Bearer",
            "jwt",
            "session_id",
            "endpoint",
            "http://",
            "https://",
            "?",
        ] {
            assert!(
                !encoded.contains(secret_shape),
                "conflict response reflected sensitive shape {secret_shape}"
            );
        }
    }

    #[test]
    fn transport_transition_conflict_is_retryable_and_origin_agnostic() {
        let body = transport_action_conflict_body();
        assert_eq!(body["error"]["code"], "qconnect_authority_transition");
        assert_eq!(body["error"]["hint"], "retry the transport command");
        let message = body["error"]["message"].as_str().unwrap();
        assert!(message.contains("handoff"));
        assert!(!message.contains("owner"));
        assert!(!message.contains("delegated"));
    }
}
