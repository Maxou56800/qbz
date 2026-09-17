//! Update only the caller's Flatpak ref through its native portal. GitHub
//! release versions do not establish availability in this installation's remote.
use crate::Result;
use futures_util::StreamExt;
use std::{
    collections::HashMap,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::Duration,
};
use zbus::{
    Connection, Proxy,
    zvariant::{OwnedObjectPath, OwnedValue, Value},
};

const SERVICE: &str = "org.freedesktop.portal.Flatpak";
const ROOT: &str = "/org/freedesktop/portal/Flatpak";
const INTERFACE: &str = "org.freedesktop.portal.Flatpak.UpdateMonitor";
static TOKEN: AtomicU64 = AtomicU64::new(0);
type Dict = HashMap<String, OwnedValue>;

#[derive(Debug, PartialEq, Eq)]
pub enum Availability {
    Current,
    Available,
    Restart,
}

#[derive(Debug)]
pub struct Info {
    pub running: String,
    pub local: String,
    pub remote: String,
}
impl Info {
    pub fn availability(&self) -> Availability {
        if self.remote != self.local {
            Availability::Available
        } else if self.local != self.running {
            Availability::Restart
        } else {
            Availability::Current
        }
    }
    pub fn reminder_key(&self) -> String {
        format!("flatpak:{}", self.remote)
    }
    fn parse(dict: &Dict) -> Result<Self> {
        fn commit(dict: &Dict, key: &str) -> Result<String> {
            let value = dict
                .get(key)
                .and_then(|v| <&str>::try_from(v).ok())
                .ok_or_else(|| format!("Flatpak omitted {key}; update availability is unknown"))?;
            if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
                return Err(format!("Flatpak returned an invalid {key}"));
            }
            Ok(value.into())
        }
        Ok(Self {
            running: commit(dict, "running-commit")?,
            local: commit(dict, "local-commit")?,
            remote: commit(dict, "remote-commit")?,
        })
    }
}

/// Owns a dedicated bus connection. Dropping the monitor disconnects its unique
/// sender, which also releases the portal object, including failed checks.
pub struct Monitor {
    proxy: Proxy<'static>,
    pub info: Info,
}
impl Monitor {
    pub async fn check() -> Result<Self> {
        tokio::time::timeout(Duration::from_secs(45), async {
            let connection = Connection::session().await.map_err(|e| e.to_string())?;
            Self::with_connection(connection).await
        })
        .await
        .map_err(|_| "Flatpak did not report update availability in time".to_string())?
    }

    async fn with_connection(connection: Connection) -> Result<Self> {
        let token = format!(
            "qbz{}_{}",
            std::process::id(),
            TOKEN.fetch_add(1, Ordering::Relaxed)
        );
        let sender = connection
            .unique_name()
            .ok_or("Flatpak connection has no unique name")?
            .as_str()
            .trim_start_matches(':')
            .replace('.', "_");
        let path = format!("{ROOT}/update_monitor/{sender}/{token}");
        let proxy = Proxy::new_owned(connection.clone(), SERVICE, path.clone(), INTERFACE)
            .await
            .map_err(|e| e.to_string())?;
        // Subscribe BEFORE creation: a locally cached update can arrive before
        // CreateUpdateMonitor returns. The caller-provided token fixes the path.
        let mut signals = proxy
            .receive_signal("UpdateAvailable")
            .await
            .map_err(|e| e.to_string())?;
        let portal = Proxy::new(&connection, SERVICE, ROOT, SERVICE)
            .await
            .map_err(|e| e.to_string())?;
        let options = HashMap::from([("handle_token", Value::from(token.as_str()))]);
        let handle: OwnedObjectPath = portal
            .call("CreateUpdateMonitor", &(options,))
            .await
            .map_err(|e| e.to_string())?;
        if handle.as_str() != path {
            return Err("Flatpak returned an unexpected update monitor".into());
        }
        let message = signals
            .next()
            .await
            .ok_or("Flatpak update monitor disconnected")?;
        let (dict,): (Dict,) = message.body().deserialize().map_err(|e| e.to_string())?;
        let info = Info::parse(&dict)?;
        drop(signals);
        Ok(Self { proxy, info })
    }

    pub async fn install(&self, cancel: &AtomicBool, progress: impl Fn(u32)) -> Result<()> {
        let operation = async {
            let mut signals = self
                .proxy
                .receive_signal("Progress")
                .await
                .map_err(|e| e.to_string())?;
            let options: HashMap<&str, Value<'_>> = HashMap::new();
            // An empty parent is permitted; Flatpak presents its own consent UI.
            self.proxy
                .call::<_, _, ()>("Update", &("", options))
                .await
                .map_err(|e| e.to_string())?;
            loop {
                let message = signals
                    .next()
                    .await
                    .ok_or("Flatpak update monitor disconnected")?;
                let (dict,): (Dict,) = message.body().deserialize().map_err(|e| e.to_string())?;
                match progress_state(&dict)? {
                    Progress::Running(percent) => progress(percent),
                    Progress::Done => return Ok(()),
                }
            }
        };
        let cancellation = async {
            while !cancel.load(Ordering::Relaxed) {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        };
        let result = tokio::select! {
            r = tokio::time::timeout(Duration::from_secs(1800), operation) =>
                r.map_err(|_| "Flatpak update timed out".to_string()).and_then(|r| r),
            _ = cancellation => Err("Update cancelled".into()),
        };
        // Close cancels the native transaction on failure/cancellation too.
        let _ = tokio::time::timeout(
            Duration::from_secs(5),
            self.proxy.call::<_, _, ()>("Close", &()),
        )
        .await;
        result
    }
}

enum Progress {
    Running(u32),
    Done,
}
fn progress_state(dict: &Dict) -> Result<Progress> {
    let uint = |key| dict.get(key).and_then(|v| u32::try_from(v).ok());
    match uint("status").unwrap_or(0) {
        0 => {
            let count = uint("n_ops").unwrap_or(1).max(1) as u64;
            let operation = uint("op").unwrap_or(0) as u64;
            let percent = uint("progress").unwrap_or(0).min(100) as u64;
            Ok(Progress::Running(
                ((operation * 100 + percent) / count).min(100) as u32,
            ))
        }
        1 => Err("Flatpak found no update to install. Check for updates again.".into()),
        2 => Ok(Progress::Done),
        3 => Err(dict
            .get("error_message")
            .and_then(|v| <&str>::try_from(v).ok())
            .unwrap_or("Flatpak could not install the update; use your software manager")
            .into()),
        _ => Err("Flatpak returned an unknown update status".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn distinguishes_channel_update_from_already_deployed_restart() {
        let mut i = Info {
            running: "a".repeat(64),
            local: "a".repeat(64),
            remote: "a".repeat(64),
        };
        assert_eq!(i.availability(), Availability::Current);
        i.remote = "b".repeat(64);
        assert_eq!(i.availability(), Availability::Available);
        i.local = i.remote.clone();
        assert_eq!(i.availability(), Availability::Restart);
        assert!(i.reminder_key().starts_with("flatpak:"));
        assert!(Info::parse(&Dict::new()).is_err());
    }
    #[test]
    fn progress_failure_is_not_install_success() {
        let d = HashMap::from([("status".into(), OwnedValue::from(3u32))]);
        assert!(progress_state(&d).is_err());
        let d = HashMap::from([("status".into(), OwnedValue::from(2u32))]);
        assert!(matches!(progress_state(&d), Ok(Progress::Done)));
    }
}

#[cfg(test)]
#[path = "flatpak_tests.rs"]
mod wire_tests;
