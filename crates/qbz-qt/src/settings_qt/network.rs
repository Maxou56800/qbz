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

const PROXY_KINDS: &[&str] = &["http", "https", "socks5"];
const PROXY_KIND_LABELS: &[&str] = &["HTTP", "HTTPS", "SOCKS5"];

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
    /// Skip TLS certificate verification for an `https`-kind proxy's own
    /// handshake only — see `qbz_net_proxy::insecure_tls`'s doc comment.
    #[serde(rename = "proxyInsecureTls")]
    pub proxy_insecure_tls: bool,
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
    /// "Block Qobuz Connect on the local network" was turned off while an
    /// active Qobuz Connect session was already running. Turning the block
    /// off has no live effect (unlike turning it on, which tears the LAN
    /// receiver down immediately) — the existing session must be
    /// disconnected and reconnected, or QBZ restarted, before local-network
    /// discovery actually comes back. Also process-lifetime only.
    #[serde(rename = "qconnectLanReconnectRecommended")]
    pub qconnect_lan_reconnect_recommended: bool,
}

#[derive(Clone, Default)]
struct TestState {
    busy: bool,
    result_kind: String,
    result_detail: String,
    restart_recommended: bool,
    qconnect_lan_reconnect_recommended: bool,
}

static TEST_STATE: std::sync::Mutex<TestState> = std::sync::Mutex::new(TestState {
    busy: false,
    result_kind: String::new(),
    result_detail: String::new(),
    restart_recommended: false,
    qconnect_lan_reconnect_recommended: false,
});

/// Translate a raw `qbz_net_proxy::test` failure into something a user can
/// act on. The raw chain (already logged in full by the caller) stays
/// technical on purpose — this is what actually renders in Settings, so it
/// only needs to cover the handful of causes a user can do something about.
fn friendly_proxy_error(raw_detail: &str, config: &qbz_net_proxy::ProxyConfig) -> String {
    let lower = raw_detail.to_lowercase();
    if lower.contains("certificate") || lower.contains("causedasendentity") {
        return if config.kind == qbz_net_proxy::ProxyKind::Https && !config.insecure_tls {
            "The proxy's TLS certificate could not be verified. If you trust this proxy \
             (for example, one using a self-signed certificate you control), enable \
             \"Skip certificate verification for this proxy\" below."
                .to_string()
        } else {
            "The proxy's TLS certificate could not be verified.".to_string()
        };
    }
    if lower.contains("proxy authorization required") || lower.contains("authoriz") {
        return "The proxy rejected the connection: authentication is required, or the \
                username/password provided is incorrect."
            .to_string();
    }
    if lower.contains("socks") && lower.contains("handshake") {
        return "The proxy did not complete the SOCKS handshake correctly. Double-check the \
                host, port and credentials."
            .to_string();
    }
    raw_detail.to_string()
}

/// The other of HTTP/HTTPS. `None` for SOCKS5 — there's no cheap "did you
/// mean the other one" check for it (it doesn't share a listening port the
/// way a plain-HTTP and a TLS-terminated proxy might get mixed up on the
/// same port number).
fn opposite_http_kind(kind: qbz_net_proxy::ProxyKind) -> Option<qbz_net_proxy::ProxyKind> {
    match kind {
        qbz_net_proxy::ProxyKind::Http => Some(qbz_net_proxy::ProxyKind::Https),
        qbz_net_proxy::ProxyKind::Https => Some(qbz_net_proxy::ProxyKind::Http),
        qbz_net_proxy::ProxyKind::Socks5 => None,
    }
}

/// A failed HTTP/HTTPS attempt often means the wrong one of the two was
/// picked for this host:port (reqwest's own error for that — a plaintext
/// CONNECT sent to a TLS-only listener, or vice versa — is an unhelpfully
/// generic "tunnel error: unsuccessful"). Rather than guess from that text,
/// confirm it: retry the SAME host/port/auth with the other kind, and only
/// report the mismatch if that retry actually succeeds — a real answer
/// instead of a hedge like "maybe you picked the wrong type".
async fn detect_http_https_mismatch(
    config: &qbz_net_proxy::ProxyConfig,
    target_url: &str,
    timeout: std::time::Duration,
) -> Option<qbz_net_proxy::ProxyKind> {
    let other_kind = opposite_http_kind(config.kind)?;
    let probe = qbz_net_proxy::ProxyConfig {
        kind: other_kind,
        host: config.host.clone(),
        port: config.port,
        auth: config.auth.clone(),
        insecure_tls: config.insecure_tls,
    };
    match qbz_net_proxy::test(&probe, target_url, timeout).await {
        qbz_net_proxy::ProxyTestOutcome::Reachable => Some(other_kind),
        _ => None,
    }
}

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
        proxy_insecure_tls: settings.proxy_insecure_tls,
        test_busy: test.busy,
        test_result_kind: test.result_kind,
        test_result_detail: test.result_detail,
        restart_recommended: test.restart_recommended,
        qconnect_lan_reconnect_recommended: test.qconnect_lan_reconnect_recommended,
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
    super::network().set_block_qconnect_lan(value)?;
    if value {
        // Take effect now, not just on the next connect(): an already-bound
        // mDNS registration / local HTTP receiver must not linger just
        // because nobody disconnected and reconnected Qobuz Connect. This
        // fully resolves the flag below for a session already caught by it,
        // since the block is enforced immediately in this direction.
        crate::qconnect_qt::stop_lan_if_running();
        clear_qconnect_lan_reconnect_recommended();
    } else if crate::qconnect_qt::is_connected() {
        // Unlike enabling it, turning the block off has no live effect: LAN
        // only (re)starts from `start_lan`, which only runs as part of
        // `connect()`. An already-running session needs a disconnect/
        // reconnect (or a restart) before local-network discovery actually
        // comes back. Cleared by `clear_qconnect_lan_reconnect_recommended`
        // once `start_lan` actually confirms the receiver is back up.
        TEST_STATE
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .qconnect_lan_reconnect_recommended = true;
    }
    Ok(())
}

/// The LAN receiver just confirmed it's up (a fresh `connect()` reached
/// `start_lan`'s success path) — any earlier "reconnect to fully apply this"
/// warning is now stale, whether it clears because the block was flipped
/// back on or because the user actually reconnected as asked.
pub(crate) fn clear_qconnect_lan_reconnect_recommended() {
    TEST_STATE
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .qconnect_lan_reconnect_recommended = false;
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
    insecure_tls: bool,
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
        store.set_proxy_insecure_tls(insecure_tls)?;
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
            log::warn!("[qbz-qt] proxy test: proxy is not enabled right after saving it");
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
            log::warn!("[qbz-qt] proxy test: failed to read back the saved proxy config: {e}");
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
        qbz_net_proxy::ProxyTestOutcome::Reachable => {
            log::info!(
                "[qbz-qt] proxy test: reachable ({}:{})",
                config.host,
                config.port
            );
            ("reachable".to_string(), String::new())
        }
        qbz_net_proxy::ProxyTestOutcome::ProxyUnreachable => {
            log::warn!(
                "[qbz-qt] proxy test: could not reach the proxy at {}:{}",
                config.host,
                config.port
            );
            (
                "proxy-unreachable".to_string(),
                "Could not reach the proxy at that host/port.".to_string(),
            )
        }
        qbz_net_proxy::ProxyTestOutcome::RequestFailed { detail, timed_out } => {
            log::warn!(
                "[qbz-qt] proxy test: request through {}:{} failed (timed_out={timed_out}): {detail}",
                config.host,
                config.port
            );
            let mismatch = detect_http_https_mismatch(
                &config,
                "https://www.qobuz.com/api.json/0.2/track/get?track_id=5966783",
                std::time::Duration::from_secs(10),
            )
            .await;
            let message = match mismatch {
                Some(other_kind) => {
                    log::info!(
                        "[qbz-qt] proxy test: {}:{} answered as {} instead of the configured {}",
                        config.host,
                        config.port,
                        other_kind.as_str(),
                        config.kind.as_str()
                    );
                    format!(
                        "This proxy answered as {} instead — switch Type to {} and try again.",
                        other_kind.as_str().to_uppercase(),
                        other_kind.as_str().to_uppercase()
                    )
                }
                None => friendly_proxy_error(&detail, &config),
            };
            ("failed".to_string(), message)
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
