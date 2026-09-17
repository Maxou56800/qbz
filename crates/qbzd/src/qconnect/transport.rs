// TODO(converge: qconnect-glue) — copied from crates/qbz/src/qconnect_transport.rs @ 00a44e12;
// do not fix bugs here without fixing the source, and vice versa.
//
//! QConnect transport config + credential discovery + device identity for the
//! qbzd daemon.
//!
//! Ports the desktop `qconnect_transport.rs` config layer. The Qobuz client is
//! reached via `runtime.core().client()` — an `Arc<RwLock<Option<QobuzClient>>>`
//! (may be `None` before the API is initialized). Two daemon adaptations vs. the
//! desktop copy (§1.4, §4): (1) the device-uuid + settings-DB path resolve
//! against the DAEMON root's `qconnect_settings.db` — its OWN device_uuid, minted
//! fresh, NEVER the desktop's global KV; (2) the default device name is
//! "QBZ (hostname)" (OD6). The KV load/persist helpers are path-parameterized
//! `*_at` variants so T11 (settings) and T13 (TUI) drive the same daemon-root DB.
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use qbz_app::shell::AppRuntime;
use qbz_qobuz::ApiError;
use qconnect_transport_ws::WsTransportConfig;
use serde::{Deserialize, Serialize};

use crate::adapter::DaemonAdapter;

const DEFAULT_QCONNECT_DEVICE_BRAND: &str = "QBZ";
const DEFAULT_QCONNECT_DEVICE_MODEL: &str = "QBZ";
const DEFAULT_QCONNECT_DEVICE_TYPE: i32 = 5; // computer
const DEFAULT_QCONNECT_SOFTWARE_PREFIX: &str = "qbz";

type Runtime = Arc<AppRuntime<DaemonAdapter>>;

#[derive(Clone, Copy)]
enum QconnectCredentialFailure {
    ApiClientUnavailable,
    Network,
    Authentication,
    Authorization,
    Offline,
    RateLimited,
    Server,
    InvalidResponse,
    ServiceResponse,
    Client,
    MissingEndpoint,
}

impl QconnectCredentialFailure {
    const fn category(self) -> &'static str {
        match self {
            Self::ApiClientUnavailable => "api-client-unavailable",
            Self::Network => "network",
            Self::Authentication => "authentication",
            Self::Authorization => "authorization",
            Self::Offline => "offline",
            Self::RateLimited => "rate-limited",
            Self::Server => "server",
            Self::InvalidResponse => "invalid-response",
            Self::ServiceResponse => "service-response",
            Self::Client => "client",
            Self::MissingEndpoint => "missing-endpoint",
        }
    }
}

fn classify_qconnect_credential_error(error: &ApiError) -> QconnectCredentialFailure {
    match error {
        ApiError::NetworkError(_) | ApiError::InvalidProxyConfig(_) => {
            QconnectCredentialFailure::Network
        }
        ApiError::AuthenticationError(_)
        | ApiError::InvalidAppId
        | ApiError::InvalidAppSecret
        | ApiError::BundleExtractionError(_) => QconnectCredentialFailure::Authentication,
        ApiError::IneligibleUser | ApiError::Forbidden(_) | ApiError::ForbiddenCircuitOpen(_) => {
            QconnectCredentialFailure::Authorization
        }
        ApiError::OfflineMode => QconnectCredentialFailure::Offline,
        ApiError::RateLimited(_) => QconnectCredentialFailure::RateLimited,
        ApiError::ServerError(_) => QconnectCredentialFailure::Server,
        ApiError::ParseError(_) => QconnectCredentialFailure::InvalidResponse,
        ApiError::ApiResponse(_) => QconnectCredentialFailure::ServiceResponse,
        ApiError::NonStreamable
        | ApiError::InvalidQuality(_)
        | ApiError::NoQualityAvailable
        | ApiError::TrackUnavailable(_) => QconnectCredentialFailure::Client,
    }
}

/// The daemon-root `qconnect_settings.db` path, set once by `qconnect::start`
/// before any connect. Daemon adaptation: the desktop delegates the path to the
/// GLOBAL `qbz_app::qconnect_identity`; the daemon points every device-identity
/// and KV read at its OWN root (§4 — the daemon never touches the desktop KV).
static DAEMON_QCONNECT_SETTINGS_DB: OnceLock<PathBuf> = OnceLock::new();

/// Point the QConnect settings/device-identity DB at the daemon root. Idempotent
/// (a second call is ignored). Called from `qconnect::start(roots)` with
/// `<roots.data>/qconnect_settings.db`.
pub fn init_settings_db_path(path: PathBuf) {
    let _ = DAEMON_QCONNECT_SETTINGS_DB.set(path);
}

/// The daemon-root QConnect settings DB path, if `init_settings_db_path` ran.
fn qconnect_settings_db_path() -> Option<PathBuf> {
    DAEMON_QCONNECT_SETTINGS_DB.get().cloned()
}

/// Persistent QConnect device identity for the DAEMON. Daemon adaptation vs. the
/// desktop (which delegates to the shared global identity module): the uuid is
/// minted + persisted in the daemon root's `qconnect_settings.db`, so the daemon
/// keeps its OWN identity, distinct from the desktop install. An explicit
/// `QBZ_QCONNECT_DEVICE_UUID` env value still wins. Fail-open: a fresh v4 when no
/// daemon settings path is available yet.
pub fn resolve_qconnect_device_uuid() -> String {
    if let Some(explicit) = std::env::var("QBZ_QCONNECT_DEVICE_UUID")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    {
        return explicit;
    }
    match qconnect_settings_db_path() {
        Some(path) => qbz_app::qconnect_identity::device_uuid_from_db(&path),
        None => uuid::Uuid::new_v4().to_string(),
    }
}

/// `qbz/<version>` software-version string for the device-info payload.
pub fn resolve_qconnect_software_version() -> String {
    std::env::var("QBZ_QCONNECT_SOFTWARE_VERSION")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| {
            format!(
                "{DEFAULT_QCONNECT_SOFTWARE_PREFIX}/{}",
                env!("CARGO_PKG_VERSION")
            )
        })
}

pub fn resolve_qconnect_device_brand() -> String {
    std::env::var("QBZ_QCONNECT_DEVICE_BRAND")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_QCONNECT_DEVICE_BRAND.to_string())
}

pub fn resolve_qconnect_device_model() -> String {
    std::env::var("QBZ_QCONNECT_DEVICE_MODEL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_QCONNECT_DEVICE_MODEL.to_string())
}

pub fn resolve_qconnect_device_type() -> i32 {
    std::env::var("QBZ_QCONNECT_DEVICE_TYPE")
        .ok()
        .and_then(|value| value.trim().parse::<i32>().ok())
        .unwrap_or(DEFAULT_QCONNECT_DEVICE_TYPE)
}

pub fn resolve_system_hostname() -> String {
    // One implementation, in qbz-app: this copy read HOSTNAME then
    // /etc/hostname, neither of which Windows has, so every Windows install
    // announced itself with the same name. The "Desktop" last resort is kept
    // here rather than pushed down: qbzd's differs, and both are user-visible.
    match qbz_app::settings::bundle::hostname().as_str() {
        "unknown" => "Desktop".to_string(),
        h => h.to_string(),
    }
}

/// Returns "QBZ ({hostname})" as the default device name (OD6 — daemon
/// adaptation; the desktop default is "Qbz - {hostname}").
pub fn resolve_default_qconnect_device_name() -> String {
    format!("QBZ ({})", resolve_system_hostname())
}

/// Effective friendly name: custom override -> env -> "QBZ ({hostname})".
pub fn resolve_qconnect_friendly_name(custom_name: Option<&str>) -> String {
    custom_name
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.to_string())
        .or_else(|| {
            std::env::var("QBZ_QCONNECT_DEVICE_NAME")
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
        .unwrap_or_else(resolve_default_qconnect_device_name)
}

// ---------------------------------------------------------------------------
// Device-info + join payload shapes (piece a). Mirror the Tauri adapter
// `types.rs` `QconnectDeviceInfoPayload` / `QconnectDeviceCapabilitiesPayload` /
// `QconnectJoinSessionRequest` exactly so the wire frames the server sees are
// byte-identical between the frontends + the daemon. Built from the device-
// identity resolvers above; the uuid resolves against the daemon root so the
// daemon keeps ONE identity of its own.
// ---------------------------------------------------------------------------

// AudioQuality wire levels: 1=mp3, 4=hires_l2(192k). Capabilities advertise the
// device's min/max decode support; VOLUME_REMOTE_CONTROL_ALLOWED(2) means a
// controller may set our volume.
pub const AUDIO_QUALITY_MP3: i32 = 1;
pub const AUDIO_QUALITY_HIRES_LEVEL2: i32 = 4;
const VOLUME_REMOTE_CONTROL_ALLOWED: i32 = 2;
/// Renderer buffer-state wire value for OK/ready (mirrors the Tauri adapter).

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QconnectDeviceCapabilitiesPayload {
    pub min_audio_quality: Option<i32>,
    pub max_audio_quality: Option<i32>,
    pub volume_remote_control: Option<i32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QconnectDeviceInfoPayload {
    pub device_uuid: Option<String>,
    pub friendly_name: Option<String>,
    pub brand: Option<String>,
    pub model: Option<String>,
    pub serial_number: Option<String>,
    pub device_type: Option<i32>,
    pub capabilities: Option<QconnectDeviceCapabilitiesPayload>,
    pub software_version: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QconnectJoinSessionRequest {
    pub session_uuid: Option<String>,
    pub device_info: Option<QconnectDeviceInfoPayload>,
}

// ---------------------------------------------------------------------------
// Controller-mode transport request DTOs (P1 CONTROLLER port). Mirror the Tauri
// adapter `types.rs` field-for-field so the wire frames the cloud parses are
// byte-identical. Do NOT rename/reorder optionals. Unused in the P0 renderer
// path (dead_code allowed) — kept so the controller port converges cleanly.
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QconnectQueueVersionPayload {
    pub major: u64,
    pub minor: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QconnectSetPlayerStateQueueItemPayload {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub queue_version: Option<QconnectQueueVersionPayload>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<i32>,
}

// `skip_serializing_if` is LOAD-BEARING here: a `None` field must be OMITTED, not
// serialized as JSON `null`. The protocol mapper rejects a `current_queue_item`
// that is present-but-null ("must be an object") and aborts the whole send, so a
// bare play/pause toggle (which sends only `playing_state`) would never reach the
// wire without these. Each SetPlayerState field is independently optional per the
// protocol, so omitting them is the intended "do one or several" behavior.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QconnectSetPlayerStateRequest {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub playing_state: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_position: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_queue_item: Option<QconnectSetPlayerStateQueueItemPayload>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QconnectSetVolumeRequest {
    pub renderer_id: Option<i32>,
    pub volume: Option<i32>,
    pub volume_delta: Option<i32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QconnectMuteVolumeRequest {
    pub renderer_id: Option<i32>,
    pub value: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QconnectSetActiveRendererRequest {
    pub renderer_id: Option<i32>,
}

/// Build a `CtrlSrvrSetPlayerState` request that seeks the remote renderer to
/// `position_ms` for `current_queue_item_id` under optimistic concurrency.
///
/// `playing_state` is intentionally `None`: a seek must not toggle play/pause.
/// Pure mirror of the Tauri `build_set_position_player_state_request`.
pub fn build_set_position_player_state_request(
    position_ms: i64,
    current_queue_item_id: Option<u64>,
    queue_version: QconnectQueueVersionPayload,
) -> QconnectSetPlayerStateRequest {
    let current_position = i32::try_from(position_ms.max(0)).ok();
    let current_queue_item = current_queue_item_id.and_then(|qid| {
        i32::try_from(qid)
            .ok()
            .map(|id| QconnectSetPlayerStateQueueItemPayload {
                queue_version: Some(QconnectQueueVersionPayload {
                    major: queue_version.major,
                    minor: queue_version.minor,
                }),
                id: Some(id),
            })
    });
    QconnectSetPlayerStateRequest {
        playing_state: None,
        current_position,
        current_queue_item,
    }
}

/// Load the persisted custom device name from the daemon-root settings DB.
/// Fail-open: None when `init_settings_db_path` never ran or on any read error.
pub(crate) fn load_persisted_device_name() -> Option<String> {
    qconnect_settings_db_path().and_then(|path| load_device_name_at(&path))
}

/// Build the device-info payload with the EFFECTIVE friendly name: the persisted
/// custom device name when one is set, else the default. The persisted name must
/// win here (not only at the controller-bootstrap call site that threads it
/// explicitly): with a custom name set, announcing the DEFAULT name from the
/// renderer join / local identity gives ONE device_uuid TWO friendly names, and
/// when the server's ADD_RENDERER omits the uuid the name-fingerprint self-match
/// fails -> `local_renderer_id = None` -> every renderer report is dropped by the
/// `is_local_renderer_active` gate (mute renderer, frozen controller seekbar).
pub fn default_qconnect_device_info() -> QconnectDeviceInfoPayload {
    default_qconnect_device_info_with_name(load_persisted_device_name().as_deref())
}

/// Build the device-info payload, overriding the friendly name when a custom
/// device name is set. Mirrors the Tauri `default_qconnect_device_info_with_name`.
pub fn default_qconnect_device_info_with_name(
    custom_name: Option<&str>,
) -> QconnectDeviceInfoPayload {
    QconnectDeviceInfoPayload {
        device_uuid: Some(resolve_qconnect_device_uuid()),
        friendly_name: Some(resolve_qconnect_friendly_name(custom_name)),
        brand: Some(resolve_qconnect_device_brand()),
        model: Some(resolve_qconnect_device_model()),
        serial_number: None,
        device_type: Some(resolve_qconnect_device_type()),
        capabilities: Some(QconnectDeviceCapabilitiesPayload {
            min_audio_quality: Some(AUDIO_QUALITY_MP3),
            max_audio_quality: Some(AUDIO_QUALITY_HIRES_LEVEL2),
            volume_remote_control: Some(VOLUME_REMOTE_CONTROL_ALLOWED),
        }),
        software_version: Some(resolve_qconnect_software_version()),
    }
}

/// Resolve THIS device's identity for injection into the frontend-agnostic
/// session-apply logic in qconnect-app (`apply_session_management_event` takes a
/// `LocalIdentity` so the crate never depends on the persistence layer). Mirrors
/// the Tauri `resolve_local_identity`.
pub fn resolve_local_identity() -> qconnect_app::LocalIdentity {
    let info = default_qconnect_device_info();
    qconnect_app::LocalIdentity {
        device_uuid: info.device_uuid.unwrap_or_default(),
        friendly_name: info.friendly_name,
        brand: info.brand,
        model: info.model,
        device_type: info.device_type,
    }
}

// ---------------------------------------------------------------------------
// Daemon-root KV helpers (path-parameterized `*_at` copies of
// qconnect_transport.rs:281-411). The desktop variants read the GLOBAL settings
// DB; the daemon threads the DB path so T11 (`qbzd qconnect enable|disable`,
// settings show/set) and T13 (TUI QConnect screen) drive the SAME daemon-root DB
// the connect path reads. Every helper is fail-open (returns the default / no-op
// on any error).
// ---------------------------------------------------------------------------

/// Open the daemon-root QConnect settings DB (key/value table), creating it when
/// missing. Mirrors the Tauri `startup.rs::open_settings_conn`.
fn open_qconnect_settings_conn_at(path: &Path) -> Option<rusqlite::Connection> {
    let conn = rusqlite::Connection::open(path).ok()?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")
        .ok()?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS settings (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL
        )",
    )
    .ok()?;
    Some(conn)
}

/// Load the persisted custom device name (fail-open: None on any error).
pub fn load_device_name_at(path: &Path) -> Option<String> {
    let conn = open_qconnect_settings_conn_at(path)?;
    conn.query_row(
        "SELECT value FROM settings WHERE key = 'device_name'",
        [],
        |row| row.get::<_, String>(0),
    )
    .ok()
    .filter(|v| !v.trim().is_empty())
}

/// Persist the custom device name (None clears it).
pub fn persist_device_name_at(path: &Path, name: Option<&str>) {
    let Some(conn) = open_qconnect_settings_conn_at(path) else {
        return;
    };
    match name {
        Some(n) => {
            let _ = conn.execute(
                "INSERT OR REPLACE INTO settings (key, value) VALUES ('device_name', ?1)",
                rusqlite::params![n],
            );
        }
        None => {
            let _ = conn.execute("DELETE FROM settings WHERE key = 'device_name'", []);
        }
    }
}

/// Load the persisted QConnect startup mode (`startup_mode` = "off" | "on" |
/// "remember_last"). Fail-open: returns `Off` (the default) when missing/invalid.
pub fn load_startup_mode_at(path: &Path) -> qconnect_app::QconnectStartupMode {
    let Some(conn) = open_qconnect_settings_conn_at(path) else {
        return qconnect_app::QconnectStartupMode::default();
    };
    conn.query_row(
        "SELECT value FROM settings WHERE key = 'startup_mode'",
        [],
        |row| row.get::<_, String>(0),
    )
    .ok()
    .as_deref()
    .and_then(qconnect_app::QconnectStartupMode::from_str)
    .unwrap_or_default()
}

/// Persist the QConnect startup mode.
pub fn save_startup_mode_at(path: &Path, mode: qconnect_app::QconnectStartupMode) {
    let Some(conn) = open_qconnect_settings_conn_at(path) else {
        return;
    };
    let _ = conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('startup_mode', ?1)",
        rusqlite::params![mode.as_str()],
    );
}

/// Load the last-known QConnect on/off state, if recorded (`last_known_state` =
/// "on" | "off").
pub fn load_last_known_state_at(path: &Path) -> Option<bool> {
    let conn = open_qconnect_settings_conn_at(path)?;
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'last_known_state'",
            [],
            |row| row.get::<_, String>(0),
        )
        .ok();
    match value.as_deref() {
        Some("on") => Some(true),
        Some("off") => Some(false),
        _ => None,
    }
}

/// Persist the last-known on/off state.
pub fn save_last_known_state_at(path: &Path, state: bool) {
    let Some(conn) = open_qconnect_settings_conn_at(path) else {
        return;
    };
    let value = if state { "on" } else { "off" };
    let _ = conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('last_known_state', ?1)",
        rusqlite::params![value],
    );
}

/// Load the daemon volume mode (`volume_mode` = "software" | "locked"; 01 §7.4).
/// NEW daemon key (the desktop never reads it). Fail-open: None when unset.
pub fn load_volume_mode_at(path: &Path) -> Option<String> {
    let conn = open_qconnect_settings_conn_at(path)?;
    conn.query_row(
        "SELECT value FROM settings WHERE key = 'volume_mode'",
        [],
        |row| row.get::<_, String>(0),
    )
    .ok()
    .filter(|v| !v.trim().is_empty())
}

/// Persist the daemon volume mode ("software" | "locked").
pub fn save_volume_mode_at(path: &Path, mode: &str) {
    let Some(conn) = open_qconnect_settings_conn_at(path) else {
        return;
    };
    let _ = conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('volume_mode', ?1)",
        rusqlite::params![mode],
    );
}

/// Load the "block QConnect LAN" safety switch (`block_lan` = "on" | "off").
/// Daemon counterpart of the desktop's Settings -> Network kill switch
/// (`qbz_app::settings::network`'s `block_qconnect_lan`) — a SEPARATE flag,
/// not a shared one: 01-architecture.md §4.2 forbids the daemon from opening
/// the desktop's data directory at runtime, so this lives in the daemon's own
/// root KV like every other qconnect.* setting. Fail-open: false (LAN
/// advertisement allowed) when missing/invalid, matching the desktop default.
pub fn load_block_lan_at(path: &Path) -> bool {
    let Some(conn) = open_qconnect_settings_conn_at(path) else {
        return false;
    };
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'block_lan'",
            [],
            |row| row.get::<_, String>(0),
        )
        .ok();
    value.as_deref() == Some("on")
}

/// Persist the "block QConnect LAN" switch.
pub fn save_block_lan_at(path: &Path, blocked: bool) {
    let Some(conn) = open_qconnect_settings_conn_at(path) else {
        return;
    };
    let value = if blocked { "on" } else { "off" };
    let _ = conn.execute(
        "INSERT OR REPLACE INTO settings (key, value) VALUES ('block_lan', ?1)",
        rusqlite::params![value],
    );
}

/// Build the WS transport config: env overrides first, then auto-discovery via
/// `qws/createToken`. Mirrors the Tauri `resolve_transport_config` (the per-option
/// knobs are deferred — the connect path uses defaults + env). `require_jwt =
/// true` (hard) and `reconnect_idle_retry_ms = 60s` match the hardened defaults.
pub async fn resolve_transport_config(runtime: &Runtime) -> Result<WsTransportConfig, String> {
    let mut endpoint_url = normalize_opt_string(std::env::var("QBZ_QCONNECT_WS_ENDPOINT").ok());
    let mut jwt_qws = normalize_opt_string(std::env::var("QBZ_QCONNECT_JWT_QWS").ok())
        .or_else(|| normalize_opt_string(std::env::var("QBZ_QCONNECT_JWT").ok()));

    if endpoint_url.is_none() || jwt_qws.is_none() {
        match fetch_qconnect_transport_credentials(runtime).await {
            Ok((discovered_endpoint, discovered_jwt_qws)) => {
                endpoint_url = endpoint_url.or(discovered_endpoint);
                jwt_qws = jwt_qws.or(discovered_jwt_qws);
            }
            Err(failure) if endpoint_url.is_some() => {
                log::warn!(
                    "[QConnect] qws/createToken auto-discovery failed, using provided endpoint \
                     (category={})",
                    failure.category()
                );
            }
            Err(failure) => {
                return Err(format!(
                    "QConnect endpoint_url is required (or QBZ_QCONNECT_WS_ENDPOINT). \
                     Auto-discovery via qws/createToken failed (category={})",
                    failure.category()
                ));
            }
        }
    }

    let endpoint_url = endpoint_url.ok_or_else(|| {
        "QConnect endpoint_url is required (or QBZ_QCONNECT_WS_ENDPOINT)".to_string()
    })?;

    let subscribe_channels = if let Ok(raw) = std::env::var("QBZ_QCONNECT_SUBSCRIBE_CHANNELS_HEX") {
        let channels: Vec<String> = raw
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(ToString::to_string)
            .collect();
        parse_subscribe_channels(channels)?
    } else {
        // Default QConnect channels: connectionId(0x01), backend(0x02), controllers(0x03)
        vec![vec![0x01], vec![0x02], vec![0x03]]
    };

    let mut config = WsTransportConfig::default();
    config.endpoint_url = endpoint_url;
    config.jwt_qws = jwt_qws;
    config.require_jwt = true;
    config.reconnect_idle_retry_ms = 60_000;
    config.subscribe_channels = subscribe_channels;
    Ok(config)
}

async fn fetch_qconnect_transport_credentials(
    runtime: &Runtime,
) -> Result<(Option<String>, Option<String>), QconnectCredentialFailure> {
    // The client is Option-wrapped (None before API init).
    let client = runtime
        .core()
        .client()
        .read()
        .await
        .clone()
        .ok_or(QconnectCredentialFailure::ApiClientUnavailable)?;

    let (endpoint, _expires_at, jwt) = client
        .create_qconnect_token()
        .await
        .map_err(|error| classify_qconnect_credential_error(&error))?
        .into_parts();
    let endpoint_url = normalize_opt_string(endpoint);
    let jwt_qws = normalize_opt_string(Some(jwt));

    if endpoint_url.is_none() {
        return Err(QconnectCredentialFailure::MissingEndpoint);
    }

    Ok((endpoint_url, jwt_qws))
}

fn normalize_opt_string(value: Option<String>) -> Option<String> {
    value.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

pub fn parse_subscribe_channels(items: Vec<String>) -> Result<Vec<Vec<u8>>, String> {
    items
        .into_iter()
        .map(|item| decode_hex_channel(&item))
        .collect()
}

pub fn decode_hex_channel(raw: &str) -> Result<Vec<u8>, String> {
    let normalized = raw.trim().trim_start_matches("0x").trim_start_matches("0X");
    if normalized.is_empty() {
        return Err("empty subscribe channel hex value".to_string());
    }

    let needs_padding = normalized.len() % 2 != 0;
    let value = if needs_padding {
        format!("0{normalized}")
    } else {
        normalized.to_string()
    };

    let mut bytes = Vec::with_capacity(value.len() / 2);
    let chars: Vec<char> = value.chars().collect();

    for idx in (0..chars.len()).step_by(2) {
        let pair = [chars[idx], chars[idx + 1]];
        let hex = pair.iter().collect::<String>();
        let byte = u8::from_str_radix(&hex, 16)
            .map_err(|_| format!("invalid subscribe channel hex byte '{hex}' in '{raw}'"))?;
        bytes.push(byte);
    }

    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_hex_channel_pads_odd_length() {
        assert_eq!(decode_hex_channel("1").unwrap(), vec![0x01]);
        assert_eq!(decode_hex_channel("0x03").unwrap(), vec![0x03]);
        assert_eq!(decode_hex_channel("0102").unwrap(), vec![0x01, 0x02]);
        assert!(decode_hex_channel("").is_err());
        assert!(decode_hex_channel("zz").is_err());
    }

    #[test]
    fn default_device_name_is_qbz_hostname_form() {
        // OD6: the daemon default is "QBZ (hostname)", never the desktop
        // "Qbz - hostname" form.
        std::env::set_var("HOSTNAME", "studio-pi");
        let name = resolve_default_qconnect_device_name();
        std::env::remove_var("HOSTNAME");
        assert_eq!(name, "QBZ (studio-pi)");
    }

    #[test]
    fn kv_helpers_round_trip_against_a_temp_daemon_db() {
        let tmp =
            std::env::temp_dir().join(format!("qbzd_qconnect_kv_test_{}.db", std::process::id()));
        let _ = std::fs::remove_file(&tmp);

        // Unset -> defaults / None.
        assert_eq!(
            load_startup_mode_at(&tmp),
            qconnect_app::QconnectStartupMode::Off
        );
        assert_eq!(load_device_name_at(&tmp), None);
        assert_eq!(load_volume_mode_at(&tmp), None);
        assert_eq!(load_last_known_state_at(&tmp), None);
        assert!(!load_block_lan_at(&tmp), "LAN advertisement allowed by default");

        // Persist + read back.
        save_startup_mode_at(&tmp, qconnect_app::QconnectStartupMode::On);
        persist_device_name_at(&tmp, Some("Living Room"));
        save_volume_mode_at(&tmp, "locked");
        save_last_known_state_at(&tmp, true);
        save_block_lan_at(&tmp, true);

        assert_eq!(
            load_startup_mode_at(&tmp),
            qconnect_app::QconnectStartupMode::On
        );
        assert_eq!(load_device_name_at(&tmp).as_deref(), Some("Living Room"));
        assert_eq!(load_volume_mode_at(&tmp).as_deref(), Some("locked"));
        assert_eq!(load_last_known_state_at(&tmp), Some(true));
        assert!(load_block_lan_at(&tmp));

        save_block_lan_at(&tmp, false);
        assert!(!load_block_lan_at(&tmp));

        // Clearing the device name removes it.
        persist_device_name_at(&tmp, None);
        assert_eq!(load_device_name_at(&tmp), None);

        let _ = std::fs::remove_file(&tmp);
    }
}
