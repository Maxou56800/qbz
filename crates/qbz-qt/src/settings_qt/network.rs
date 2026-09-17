//! Settings > Network — the outgoing HTTP(S)/SOCKS proxy and the QConnect
//! LAN / Cast safety switches.
//!
//! The proxy itself is `qbz_app::settings::network` (persisted, global —
//! see that module's docs); this file is the Settings-panel view over it
//! plus the one piece that module doesn't own: pushing the live
//! `qbz_net_proxy::ProxyConfig` into the process-wide registry every
//! consumer in the workspace reads (`qbz_net_proxy::apply_current`).
//!
//! Host/port/kind/auth/username/password are staged in QML and committed in
//! one action (`test_and_save`), like Plex's `plexConnect(url, token)`: an
//! empty password field means "keep the saved one", not "clear it" — use
//! `clear_password` for that. The two safety toggles and the master enable
//! switch apply instantly, like every other Settings toggle.

use serde::Serialize;

const PROXY_KINDS: &[&str] = &["http", "https", "socks4", "socks5"];
const PROXY_KIND_LABELS: &[&str] = &["HTTP", "HTTPS", "SOCKS4", "SOCKS5"];

#[derive(Clone, Default, Serialize)]
pub struct Snapshot {
    #[serde(rename = "proxyEnabled")]
    pub proxy_enabled: bool,
    #[serde(rename = "proxyKindOptions")]
    pub proxy_kind_options: Vec<String>,
    #[serde(rename = "proxyKindIndex")]
    pub proxy_kind_index: i32,
    #[serde(rename = "proxyHost")]
    pub proxy_host: String,
    #[serde(rename = "proxyPort")]
    pub proxy_port: i32,
    #[serde(rename = "proxyAuthEnabled")]
    pub proxy_auth_enabled: bool,
    #[serde(rename = "proxyUsername")]
    pub proxy_username: String,
    #[serde(rename = "proxyHasPassword")]
    pub proxy_has_password: bool,
    #[serde(rename = "blockQconnectLan")]
    pub block_qconnect_lan: bool,
    #[serde(rename = "blockCast")]
    pub block_cast: bool,
    /// A test_and_save() is running — the button shows a spinner and further
    /// clicks are ignored (guarded by `TEST_BUSY`, not just the UI).
    #[serde(rename = "testBusy")]
    pub test_busy: bool,
    /// "" (no test run yet) | "reachable" | "proxy-unreachable" | "failed".
    /// QML picks the icon/copy off this; `testResultDetail` is the raw
    /// diagnostic text for an expandable "details" line, not for matching on.
    #[serde(rename = "testResultKind")]
    pub test_result_kind: String,
    #[serde(rename = "testResultDetail")]
    pub test_result_detail: String,
    /// The proxy was enabled, disabled or reconfigured since QBZ started.
    /// Not persisted (resets to false on every launch): it exists to tell the
    /// user a restart is needed for the account session and Qobuz Connect to
    /// pick up the change — see `client.rs`'s "applied once, at construction"
    /// comment for why this can't just apply itself live.
    #[serde(rename = "restartRecommended")]
    pub restart_recommended: bool,
}

#[derive(Clone, Default)]
struct TestState {
    busy: bool,
    result_kind: String,
    result_detail: String,
    restart_recommended: bool,
}

static TEST_STATE: std::sync::Mutex<TestState> = std::sync::Mutex::new(TestState {
    busy: false,
    result_kind: String::new(),
    result_detail: String::new(),
    restart_recommended: false,
});

/// Read the current proxy setting and push it into the process-wide
/// registry every HTTP client (and the QConnect WebSocket tunnel) reads.
/// Called once at startup (`crate::settings_qt::seed_network_proxy`) and
/// again after every change made here.
fn apply_proxy_config() {
    match super::network().proxy_config() {
        Ok(config) => qbz_net_proxy::set_current(config),
        Err(e) => log::warn!("[qbz-qt] failed to read network settings: {e}"),
    }
}

pub fn snapshot() -> Snapshot {
    let settings = super::network().get_settings().unwrap_or_default();
    let kind_index = PROXY_KINDS
        .iter()
        .position(|kind| *kind == settings.proxy_kind)
        .unwrap_or(0) as i32;
    let test = TEST_STATE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();

    Snapshot {
        proxy_enabled: settings.proxy_enabled,
        proxy_kind_options: PROXY_KIND_LABELS.iter().map(|s| s.to_string()).collect(),
        proxy_kind_index: kind_index,
        proxy_host: settings.proxy_host,
        proxy_port: settings.proxy_port as i32,
        proxy_auth_enabled: settings.proxy_auth_enabled,
        proxy_username: settings.proxy_username,
        proxy_has_password: settings.proxy_has_password,
        block_qconnect_lan: settings.block_qconnect_lan,
        block_cast: settings.block_cast,
        test_busy: test.busy,
        test_result_kind: test.result_kind,
        test_result_detail: test.result_detail,
        restart_recommended: test.restart_recommended,
    }
}

/// Called once at startup, before login (the setting is global — see the
/// module docs), so a configured proxy protects the very first request.
pub fn seed() {
    apply_proxy_config();
}

pub fn set_proxy_enabled(value: bool) -> Result<(), String> {
    super::network().set_proxy_enabled(value)?;
    apply_proxy_config();
    mark_restart_recommended();
    Ok(())
}

/// Flag that the account session / Qobuz Connect won't see the current proxy
/// config until QBZ restarts (see `Snapshot::restart_recommended`'s doc).
fn mark_restart_recommended() {
    TEST_STATE
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .restart_recommended = true;
}

pub fn set_block_qconnect_lan(value: bool) -> Result<(), String> {
    super::network().set_block_qconnect_lan(value)
}

pub fn set_block_cast(value: bool) -> Result<(), String> {
    super::network().set_block_cast(value)
}

/// "Clear saved password" — the only way to remove one; leaving the field
/// blank in `test_and_save` keeps the existing password unchanged.
pub fn clear_password() {
    if let Err(e) = super::network().set_proxy_password("") {
        log::warn!("[qbz-qt] failed to clear proxy password: {e}");
        return;
    }
    apply_proxy_config();
    mark_restart_recommended();
}

/// Persist every staged field, apply it, and test reachability against a
/// Qobuz endpoint. Runs on the tokio runtime (spawned by the caller) since
/// `qbz_net_proxy::test` does real I/O; this function itself does not spawn.
pub async fn test_and_save(
    kind_index: i32,
    host: String,
    port: i32,
    auth_enabled: bool,
    username: String,
    password: String,
) {
    {
        let mut test = TEST_STATE.lock().unwrap_or_else(|p| p.into_inner());
        test.busy = true;
        test.result_kind.clear();
        test.result_detail.clear();
    }
    crate::settings_qt::publish_snapshot().await;

    let kind = PROXY_KINDS
        .get(kind_index.max(0) as usize)
        .copied()
        .unwrap_or("http");
    let store = super::network();
    let persisted = (|| -> Result<(), String> {
        store.set_proxy_kind(kind)?;
        store.set_proxy_host(host.trim())?;
        store.set_proxy_port(port.clamp(1, u16::MAX as i32) as u16)?;
        store.set_proxy_auth_enabled(auth_enabled)?;
        store.set_proxy_username(username.trim())?;
        if !password.is_empty() {
            store.set_proxy_password(&password)?;
        }
        store.set_proxy_enabled(true)
    })();

    if let Err(e) = persisted {
        log::warn!("[qbz-qt] failed to persist network settings: {e}");
        {
            let mut test = TEST_STATE.lock().unwrap_or_else(|p| p.into_inner());
            test.busy = false;
            test.result_kind = "failed".to_string();
            test.result_detail = e;
        }
        crate::settings_qt::publish_snapshot().await;
        return;
    }
    apply_proxy_config();
    mark_restart_recommended();

    let config = match store.proxy_config() {
        Ok(Some(config)) => config,
        Ok(None) => {
            // Shouldn't happen right after set_proxy_enabled(true) with a
            // non-empty host, but treat it the same as any other failure
            // rather than unwrap-panicking on a settings-store race.
            {
                let mut test = TEST_STATE.lock().unwrap_or_else(|p| p.into_inner());
                test.busy = false;
                test.result_kind = "failed".to_string();
                test.result_detail = "proxy is not enabled after saving".to_string();
            }
            crate::settings_qt::publish_snapshot().await;
            return;
        }
        Err(e) => {
            {
                let mut test = TEST_STATE.lock().unwrap_or_else(|p| p.into_inner());
                test.busy = false;
                test.result_kind = "failed".to_string();
                test.result_detail = e;
            }
            crate::settings_qt::publish_snapshot().await;
            return;
        }
    };

    // A Qobuz endpoint, not a generic reachability check: the point is "can
    // qbz reach Qobuz through this proxy", not "is the internet up".
    let outcome = qbz_net_proxy::test(
        &config,
        "https://www.qobuz.com/api.json/0.2/track/get?track_id=5966783",
        std::time::Duration::from_secs(10),
    )
    .await;

    let (kind, detail) = match outcome {
        qbz_net_proxy::ProxyTestOutcome::Reachable => ("reachable".to_string(), String::new()),
        qbz_net_proxy::ProxyTestOutcome::ProxyUnreachable => (
            "proxy-unreachable".to_string(),
            "Could not reach the proxy at that host/port.".to_string(),
        ),
        qbz_net_proxy::ProxyTestOutcome::RequestFailed { detail, .. } => {
            ("failed".to_string(), detail)
        }
    };
    {
        let mut test = TEST_STATE.lock().unwrap_or_else(|p| p.into_inner());
        test.busy = false;
        test.result_kind = kind;
        test.result_detail = detail;
    }
    crate::settings_qt::publish_snapshot().await;
}
