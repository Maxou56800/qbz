//! Process-wide "current proxy" registry.
//!
//! [`crate::apply`] is the low-level primitive: it takes the config as a
//! parameter, which is the right shape for a crate with no opinion about
//! *where* that config lives. But threading a `ProxyConfig` parameter through
//! every `reqwest::Client::builder()` call site in the workspace — Qobuz,
//! three media-server integrations, four scrobbler/metadata integrations,
//! the updater, and every ad-hoc download helper in qbz-qt/qbzd — would mean
//! changing the public constructor of every one of those, in crates that are
//! deliberately decoupled from `qbz-app` (where the setting is persisted) and
//! from each other.
//!
//! So this module holds the config instead, set once by whoever owns
//! `qbz_app::settings::network` (qbz-qt/qbzd, at startup and whenever the
//! Network settings panel changes it) and read by every call site through
//! [`apply_current`]. A call site adds one dependency and changes one line;
//! it never needs to know the setting exists as a *setting*.

use std::sync::RwLock;

use reqwest::ClientBuilder;

use crate::{apply, ProxyConfig, ProxyConfigError};

struct Current {
    generation: u64,
    config: Option<ProxyConfig>,
}

fn cell() -> &'static RwLock<Current> {
    static CURRENT: std::sync::OnceLock<RwLock<Current>> = std::sync::OnceLock::new();
    CURRENT.get_or_init(|| {
        RwLock::new(Current {
            generation: 0,
            config: None,
        })
    })
}

/// Replace the process-wide proxy configuration. `None` means disabled.
pub fn set_current(config: Option<ProxyConfig>) {
    let mut guard = cell()
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.generation += 1;
    guard.config = config;
}

/// Opaque marker for the current proxy configuration. The handful of client
/// construction sites in the workspace that cache their `reqwest::Client`
/// instead of rebuilding it per call (a `static OnceLock`/`LazyLock`, or one
/// held in a long-lived struct field) store this alongside the client and
/// rebuild when it no longer matches [`current_generation`], instead of
/// serving a client built against a proxy the user has since changed or
/// turned off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Generation(u64);

pub fn current_generation() -> Generation {
    Generation(
        cell()
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .generation,
    )
}

/// Apply the current process-wide proxy to a client builder. Every
/// `reqwest::Client::builder()` call site in the workspace should call this
/// in place of building unproxied.
pub fn apply_current(builder: ClientBuilder) -> Result<ClientBuilder, ProxyConfigError> {
    let guard = cell()
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    apply(builder, guard.config.as_ref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProxyKind;
    use std::sync::Mutex;

    // set_current mutates process-wide state; serialize this file's tests so
    // they can't observe each other's writes.
    static SERIAL: Mutex<()> = Mutex::new(());

    #[test]
    fn starts_disabled() {
        let _lock = SERIAL.lock().unwrap();
        set_current(None);
        assert!(apply_current(ClientBuilder::new()).is_ok());
    }

    #[test]
    fn setting_a_config_bumps_the_generation() {
        let _lock = SERIAL.lock().unwrap();
        set_current(None);
        let before = current_generation();
        set_current(Some(ProxyConfig {
            kind: ProxyKind::Http,
            host: "proxy.example.com".to_string(),
            port: 8080,
            auth: None,
        }));
        assert_ne!(before, current_generation());
    }

    #[test]
    fn clearing_back_to_none_also_bumps_the_generation() {
        let _lock = SERIAL.lock().unwrap();
        set_current(Some(ProxyConfig {
            kind: ProxyKind::Http,
            host: "proxy.example.com".to_string(),
            port: 8080,
            auth: None,
        }));
        let before = current_generation();
        set_current(None);
        assert_ne!(before, current_generation());
        assert!(apply_current(ClientBuilder::new()).is_ok());
    }
}
