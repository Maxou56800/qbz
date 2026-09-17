//! Global network/proxy preference storage.
//!
//! Machine-wide, not per Qobuz account — opened once via a process-wide
//! `OnceLock` and never re-bound per user, exactly like
//! [`super::playback::PlaybackPreferencesState`]. A proxy choice, and the LAN
//!/cast safety switches that live alongside it, must survive account
//! switches and be readable before any login happens.
//!
//! The proxy password is never stored in plaintext: it is wrapped with
//! `qbz-secrets` (OS keyring master key, or the device-bound KDF fallback)
//! before it touches disk, and it never rides [`NetworkSettings`] itself —
//! callers that need it call [`NetworkSettingsStore::proxy_config`], which
//! decrypts it fresh for exactly as long as it takes to build one
//! `qbz_net_proxy::ProxyConfig`.

use log::info;
use qbz_net_proxy::{ProxyAuth, ProxyConfig, ProxyKind};
use qbz_secrets::SecretBox;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

const SECRET_SERVICE_NAME: &str = "qbz-network";

fn default_proxy_kind() -> String {
    "http".to_string()
}

fn default_proxy_port() -> u16 {
    1080
}

/// Coerce a free-form stored value to a supported [`ProxyKind`] string.
/// Anything unrecognized (a downgrade reading a newer kind, DB corruption)
/// falls back to `"http"` rather than failing the whole settings read.
fn normalize_proxy_kind(value: &str) -> String {
    ProxyKind::from_str(value)
        .map(|kind| kind.as_str().to_string())
        .unwrap_or_else(|_| default_proxy_kind())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NetworkSettings {
    pub proxy_enabled: bool,
    #[serde(default = "default_proxy_kind")]
    pub proxy_kind: String,
    #[serde(default)]
    pub proxy_host: String,
    #[serde(default = "default_proxy_port")]
    pub proxy_port: u16,
    #[serde(default)]
    pub proxy_auth_enabled: bool,
    #[serde(default)]
    pub proxy_username: String,
    /// A password has been saved. The password itself is never in this
    /// struct — see the module docs.
    #[serde(default)]
    pub proxy_has_password: bool,
    /// Force-disable Qobuz Connect's LAN surface (mDNS advertisement + local
    /// HTTP receiver), regardless of the player-bar Connect toggle or
    /// auto-connect-on-startup.
    #[serde(default)]
    pub block_qconnect_lan: bool,
    /// Force-disable Chromecast/DLNA discovery and the local cast media
    /// server.
    #[serde(default)]
    pub block_cast: bool,
    /// Skip TLS certificate verification for an `https` proxy's own
    /// handshake — never for the real target reached through it. Only
    /// meaningful when `proxy_kind == "https"`; see
    /// `qbz_net_proxy::insecure_tls`'s doc comment for why this can't be
    /// reqwest's `danger_accept_invalid_certs` (that disables verification
    /// for the whole client, Qobuz's own API included).
    #[serde(default)]
    pub proxy_insecure_tls: bool,
}

impl Default for NetworkSettings {
    fn default() -> Self {
        Self {
            proxy_enabled: false,
            proxy_kind: default_proxy_kind(),
            proxy_host: String::new(),
            proxy_port: default_proxy_port(),
            proxy_auth_enabled: false,
            proxy_username: String::new(),
            proxy_has_password: false,
            block_qconnect_lan: false,
            block_cast: false,
            proxy_insecure_tls: false,
        }
    }
}

pub struct NetworkSettingsStore {
    conn: Connection,
    vault: SecretBox,
}

impl NetworkSettingsStore {
    fn open_at(dir: &Path, db_name: &str) -> Result<Self, String> {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("Failed to create data directory: {}", e))?;

        let db_path = dir.join(db_name);
        let conn = Connection::open(&db_path)
            .map_err(|e| format!("Failed to open network settings database: {}", e))?;

        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA busy_timeout=1000;",
        )
        .map_err(|e| format!("Failed to enable WAL for network settings database: {}", e))?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS network_settings (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                proxy_enabled INTEGER NOT NULL DEFAULT 0,
                proxy_kind TEXT NOT NULL DEFAULT 'http',
                proxy_host TEXT NOT NULL DEFAULT '',
                proxy_port INTEGER NOT NULL DEFAULT 1080,
                proxy_auth_enabled INTEGER NOT NULL DEFAULT 0,
                proxy_username TEXT NOT NULL DEFAULT '',
                proxy_password_wrapped BLOB,
                block_qconnect_lan INTEGER NOT NULL DEFAULT 0,
                block_cast INTEGER NOT NULL DEFAULT 0
            );",
        )
        .map_err(|e| format!("Failed to create network settings table: {}", e))?;

        let _ = conn.execute_batch(
            "ALTER TABLE network_settings ADD COLUMN proxy_insecure_tls INTEGER NOT NULL DEFAULT 0;",
        );

        conn.execute("INSERT OR IGNORE INTO network_settings (id) VALUES (1)", [])
            .map_err(|e| format!("Failed to insert default network settings: {}", e))?;

        // The keyring/KDF-fallback selection happens inside qbz-secrets; the
        // install UUID for the KDF fallback lives in the same directory as
        // the settings DB so it survives upgrades.
        let vault = SecretBox::open(SECRET_SERVICE_NAME, dir)
            .map_err(|e| format!("Failed to open the network secret vault: {}", e))?;

        info!("[NetworkSettings] Database initialized");

        Ok(Self { conn, vault })
    }

    pub fn new() -> Result<Self, String> {
        let data_dir = dirs::data_dir()
            .ok_or("Could not determine data directory")?
            .join("qbz");
        Self::open_at(&data_dir, "network_settings.db")
    }

    pub fn new_at(base_dir: &Path) -> Result<Self, String> {
        Self::open_at(base_dir, "network_settings.db")
    }

    pub fn get_settings(&self) -> Result<NetworkSettings, String> {
        self.conn
            .query_row(
                "SELECT proxy_enabled, proxy_kind, proxy_host, proxy_port, proxy_auth_enabled,
                        proxy_username, proxy_password_wrapped IS NOT NULL, block_qconnect_lan,
                        block_cast, proxy_insecure_tls
                 FROM network_settings WHERE id = 1",
                [],
                |row| {
                    let proxy_enabled: i32 = row.get(0)?;
                    let proxy_kind: String = row.get(1)?;
                    let proxy_host: String = row.get(2)?;
                    let proxy_port: u16 = row.get(3)?;
                    let proxy_auth_enabled: i32 = row.get(4)?;
                    let proxy_username: String = row.get(5)?;
                    let proxy_has_password: bool = row.get(6)?;
                    let block_qconnect_lan: i32 = row.get(7)?;
                    let block_cast: i32 = row.get(8)?;
                    let proxy_insecure_tls: i32 = row.get(9)?;
                    Ok(NetworkSettings {
                        proxy_enabled: proxy_enabled != 0,
                        proxy_kind: normalize_proxy_kind(&proxy_kind),
                        proxy_host,
                        proxy_port,
                        proxy_auth_enabled: proxy_auth_enabled != 0,
                        proxy_username,
                        proxy_has_password,
                        block_qconnect_lan: block_qconnect_lan != 0,
                        block_cast: block_cast != 0,
                        proxy_insecure_tls: proxy_insecure_tls != 0,
                    })
                },
            )
            .map_err(|e| format!("Failed to get network settings: {}", e))
    }

    pub fn set_proxy_enabled(&self, value: bool) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE network_settings SET proxy_enabled = ?1 WHERE id = 1",
                params![if value { 1 } else { 0 }],
            )
            .map_err(|e| format!("Failed to set proxy_enabled: {}", e))?;
        Ok(())
    }

    pub fn set_proxy_kind(&self, value: &str) -> Result<(), String> {
        let normalized = normalize_proxy_kind(value);
        self.conn
            .execute(
                "UPDATE network_settings SET proxy_kind = ?1 WHERE id = 1",
                params![normalized],
            )
            .map_err(|e| format!("Failed to set proxy_kind: {}", e))?;
        Ok(())
    }

    pub fn set_proxy_host(&self, value: &str) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE network_settings SET proxy_host = ?1 WHERE id = 1",
                params![value],
            )
            .map_err(|e| format!("Failed to set proxy_host: {}", e))?;
        Ok(())
    }

    pub fn set_proxy_port(&self, value: u16) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE network_settings SET proxy_port = ?1 WHERE id = 1",
                params![value],
            )
            .map_err(|e| format!("Failed to set proxy_port: {}", e))?;
        Ok(())
    }

    pub fn set_proxy_auth_enabled(&self, value: bool) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE network_settings SET proxy_auth_enabled = ?1 WHERE id = 1",
                params![if value { 1 } else { 0 }],
            )
            .map_err(|e| format!("Failed to set proxy_auth_enabled: {}", e))?;
        Ok(())
    }

    pub fn set_proxy_username(&self, value: &str) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE network_settings SET proxy_username = ?1 WHERE id = 1",
                params![value],
            )
            .map_err(|e| format!("Failed to set proxy_username: {}", e))?;
        Ok(())
    }

    /// Wrap and persist a new proxy password. An empty value clears it.
    pub fn set_proxy_password(&self, value: &str) -> Result<(), String> {
        if value.is_empty() {
            self.conn
                .execute(
                    "UPDATE network_settings SET proxy_password_wrapped = NULL WHERE id = 1",
                    [],
                )
                .map_err(|e| format!("Failed to clear proxy password: {}", e))?;
            return Ok(());
        }
        let wrapped = self
            .vault
            .wrap(value.as_bytes())
            .map_err(|e| format!("Failed to wrap proxy password: {}", e))?;
        self.conn
            .execute(
                "UPDATE network_settings SET proxy_password_wrapped = ?1 WHERE id = 1",
                params![wrapped],
            )
            .map_err(|e| format!("Failed to set proxy password: {}", e))?;
        Ok(())
    }

    fn proxy_password(&self) -> Result<Option<String>, String> {
        let wrapped: Option<Vec<u8>> = self
            .conn
            .query_row(
                "SELECT proxy_password_wrapped FROM network_settings WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to read proxy password: {}", e))?;
        let Some(wrapped) = wrapped else {
            return Ok(None);
        };
        let plaintext = self
            .vault
            .unwrap(&wrapped)
            .map_err(|e| format!("Failed to unwrap proxy password: {}", e))?;
        String::from_utf8(plaintext)
            .map(Some)
            .map_err(|e| format!("Corrupt proxy password: {}", e))
    }

    pub fn set_block_qconnect_lan(&self, value: bool) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE network_settings SET block_qconnect_lan = ?1 WHERE id = 1",
                params![if value { 1 } else { 0 }],
            )
            .map_err(|e| format!("Failed to set block_qconnect_lan: {}", e))?;
        Ok(())
    }

    pub fn set_block_cast(&self, value: bool) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE network_settings SET block_cast = ?1 WHERE id = 1",
                params![if value { 1 } else { 0 }],
            )
            .map_err(|e| format!("Failed to set block_cast: {}", e))?;
        Ok(())
    }

    pub fn set_proxy_insecure_tls(&self, value: bool) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE network_settings SET proxy_insecure_tls = ?1 WHERE id = 1",
                params![if value { 1 } else { 0 }],
            )
            .map_err(|e| format!("Failed to set proxy_insecure_tls: {}", e))?;
        Ok(())
    }

    /// The live proxy configuration for `qbz_net_proxy::apply`, or `None`
    /// when the proxy is off. Decrypts the password fresh on every call so a
    /// change is picked up without restarting the process; callers that
    /// build a client per request (the common case in this workspace) are
    /// expected to call this each time rather than cache the result.
    pub fn proxy_config(&self) -> Result<Option<ProxyConfig>, String> {
        let settings = self.get_settings()?;
        if !settings.proxy_enabled {
            return Ok(None);
        }
        if settings.proxy_host.trim().is_empty() {
            // "Enabled" with no host set yet (e.g. mid-edit in Settings,
            // before Save/Test is pressed) must behave as disabled, not as a
            // config that is guaranteed to fail every client in the
            // workspace that calls qbz_net_proxy::apply_current.
            log::warn!("[NetworkSettings] proxy is enabled but has no host set; treating as disabled");
            return Ok(None);
        }
        let kind = ProxyKind::from_str(&settings.proxy_kind).map_err(|e| e.to_string())?;
        let auth = if settings.proxy_auth_enabled {
            Some(ProxyAuth {
                username: settings.proxy_username,
                password: self.proxy_password()?.unwrap_or_default(),
            })
        } else {
            None
        };
        Ok(Some(ProxyConfig {
            kind,
            host: settings.proxy_host,
            port: settings.proxy_port,
            auth,
            insecure_tls: settings.proxy_insecure_tls,
        }))
    }
}

pub struct NetworkSettingsState {
    pub store: Arc<Mutex<Option<NetworkSettingsStore>>>,
}

impl NetworkSettingsState {
    pub fn new() -> Result<Self, String> {
        let store = NetworkSettingsStore::new()?;
        Ok(Self {
            store: Arc::new(Mutex::new(Some(store))),
        })
    }

    pub fn new_empty() -> Self {
        Self {
            store: Arc::new(Mutex::new(None)),
        }
    }

    pub fn init_at(&self, base_dir: &Path) -> Result<(), String> {
        let new_store = NetworkSettingsStore::new_at(base_dir)?;
        let mut guard = self
            .store
            .lock()
            .map_err(|_| "Failed to lock network settings store".to_string())?;
        *guard = Some(new_store);
        Ok(())
    }

    fn with_store<T>(
        &self,
        f: impl FnOnce(&NetworkSettingsStore) -> Result<T, String>,
    ) -> Result<T, String> {
        let guard = self
            .store
            .lock()
            .map_err(|_| "Failed to lock network settings store".to_string())?;
        let store = guard.as_ref().ok_or("Network settings store is not open")?;
        f(store)
    }

    pub fn get_settings(&self) -> Result<NetworkSettings, String> {
        self.with_store(|s| s.get_settings())
    }

    pub fn set_proxy_enabled(&self, value: bool) -> Result<(), String> {
        self.with_store(|s| s.set_proxy_enabled(value))
    }

    pub fn set_proxy_kind(&self, value: &str) -> Result<(), String> {
        self.with_store(|s| s.set_proxy_kind(value))
    }

    pub fn set_proxy_host(&self, value: &str) -> Result<(), String> {
        self.with_store(|s| s.set_proxy_host(value))
    }

    pub fn set_proxy_port(&self, value: u16) -> Result<(), String> {
        self.with_store(|s| s.set_proxy_port(value))
    }

    pub fn set_proxy_auth_enabled(&self, value: bool) -> Result<(), String> {
        self.with_store(|s| s.set_proxy_auth_enabled(value))
    }

    pub fn set_proxy_username(&self, value: &str) -> Result<(), String> {
        self.with_store(|s| s.set_proxy_username(value))
    }

    pub fn set_proxy_password(&self, value: &str) -> Result<(), String> {
        self.with_store(|s| s.set_proxy_password(value))
    }

    pub fn set_block_qconnect_lan(&self, value: bool) -> Result<(), String> {
        self.with_store(|s| s.set_block_qconnect_lan(value))
    }

    pub fn set_block_cast(&self, value: bool) -> Result<(), String> {
        self.with_store(|s| s.set_block_cast(value))
    }

    pub fn set_proxy_insecure_tls(&self, value: bool) -> Result<(), String> {
        self.with_store(|s| s.set_proxy_insecure_tls(value))
    }

    pub fn proxy_config(&self) -> Result<Option<ProxyConfig>, String> {
        self.with_store(|s| s.proxy_config())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_test_dir(name: &str) -> std::path::PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("qbz-app-{name}-{}-{nonce}", std::process::id()))
    }

    fn fresh_store(name: &str) -> (std::path::PathBuf, NetworkSettingsStore) {
        let dir = unique_test_dir(name);
        let store = NetworkSettingsStore::new_at(&dir).expect("open store in temp dir");
        (dir, store)
    }

    #[test]
    fn defaults_are_stable_and_proxy_starts_disabled() {
        let settings = NetworkSettings::default();
        assert!(!settings.proxy_enabled);
        assert_eq!(settings.proxy_kind, "http");
        assert_eq!(settings.proxy_port, 1080);
        assert!(!settings.proxy_has_password);
        assert!(!settings.block_qconnect_lan);
        assert!(!settings.block_cast);
        assert!(!settings.proxy_insecure_tls);
    }

    #[test]
    fn store_returns_defaults() {
        let (dir, store) = fresh_store("network-default");
        let settings = store.get_settings().expect("get settings");
        assert_eq!(settings, NetworkSettings::default());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn persists_all_fields_across_a_reopen() {
        let dir = unique_test_dir("network-persist");
        {
            let store = NetworkSettingsStore::new_at(&dir).expect("open store");
            store.set_proxy_enabled(true).expect("set enabled");
            store.set_proxy_kind("socks5").expect("set kind");
            store.set_proxy_host("proxy.example.com").expect("set host");
            store.set_proxy_port(9050).expect("set port");
            store.set_proxy_auth_enabled(true).expect("set auth flag");
            store.set_proxy_username("alice").expect("set username");
            store.set_proxy_password("hunter2").expect("set password");
            store.set_block_qconnect_lan(true).expect("set lan block");
            store.set_block_cast(true).expect("set cast block");
            store
                .set_proxy_insecure_tls(true)
                .expect("set insecure tls");
        }

        let reopened = NetworkSettingsStore::new_at(&dir).expect("reopen store");
        let settings = reopened.get_settings().expect("get settings");
        assert!(settings.proxy_enabled);
        assert_eq!(settings.proxy_kind, "socks5");
        assert_eq!(settings.proxy_host, "proxy.example.com");
        assert_eq!(settings.proxy_port, 9050);
        assert!(settings.proxy_auth_enabled);
        assert_eq!(settings.proxy_username, "alice");
        assert!(settings.proxy_has_password);
        assert!(settings.block_qconnect_lan);
        assert!(settings.block_cast);
        assert!(settings.proxy_insecure_tls);
        // The wrapped blob's bytes persisted (proxy_has_password, checked
        // above) — that's this module's responsibility. Whether the SAME
        // ciphertext still decrypts after a fresh SecretBox::open() is
        // qbz-secrets' own guarantee, and its test suite deliberately tests
        // it across a real process restart (tests/survives_a_restart.rs),
        // not by reopening in-process: a keyring backend that silently falls
        // back to an in-memory mock (no real OS keyring reachable, e.g. this
        // sandbox) hands back a fresh, unrelated key on every open, which
        // would make an in-process reopen-and-decrypt assertion here flaky
        // for a reason that has nothing to do with this module.
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn clearing_the_password_removes_it() {
        let dir = unique_test_dir("network-clear-password");
        let store = NetworkSettingsStore::new_at(&dir).expect("open store");
        store.set_proxy_password("hunter2").expect("set password");
        assert!(store.get_settings().unwrap().proxy_has_password);

        store.set_proxy_password("").expect("clear password");
        assert!(!store.get_settings().unwrap().proxy_has_password);
        assert_eq!(store.proxy_password().expect("read password"), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn unknown_proxy_kind_falls_back_to_http() {
        assert_eq!(normalize_proxy_kind("wireguard"), "http");
        assert_eq!(normalize_proxy_kind("socks5"), "socks5");
    }

    #[test]
    fn proxy_config_is_none_while_disabled() {
        let (dir, store) = fresh_store("network-config-disabled");
        store.set_proxy_host("proxy.example.com").expect("set host");
        assert!(store.proxy_config().expect("config").is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn proxy_config_is_none_when_enabled_with_no_host() {
        // Enabled mid-edit, before a host is entered, must not surface a
        // config that fails every client in the workspace that reads it.
        let (dir, store) = fresh_store("network-config-no-host");
        store.set_proxy_enabled(true).expect("enable");
        assert!(store.proxy_config().expect("config").is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn proxy_config_carries_decrypted_auth_when_enabled() {
        let (dir, store) = fresh_store("network-config-enabled");
        store.set_proxy_enabled(true).expect("enable");
        store.set_proxy_kind("socks5").expect("kind");
        store.set_proxy_host("proxy.example.com").expect("host");
        store.set_proxy_port(1080).expect("port");
        store.set_proxy_auth_enabled(true).expect("auth flag");
        store.set_proxy_username("alice").expect("username");
        store.set_proxy_password("hunter2").expect("password");

        let config = store
            .proxy_config()
            .expect("config")
            .expect("proxy enabled");
        assert_eq!(config.kind, ProxyKind::Socks5);
        assert_eq!(config.host, "proxy.example.com");
        assert_eq!(config.port, 1080);
        let auth = config.auth.expect("auth present");
        assert_eq!(auth.username, "alice");
        assert_eq!(auth.password, "hunter2");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn state_requires_an_open_store() {
        let state = NetworkSettingsState::new_empty();
        assert!(state.get_settings().is_err());

        let dir = unique_test_dir("network-state-init");
        state.init_at(&dir).expect("init at temp dir");
        assert_eq!(state.get_settings().unwrap(), NetworkSettings::default());
        let _ = std::fs::remove_dir_all(dir);
    }
}
