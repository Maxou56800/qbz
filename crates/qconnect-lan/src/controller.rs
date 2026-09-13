//! The CONTROLLER half of the official LAN wire (2026-09-13).
//!
//! `server.rs` is what official controllers call on QBZ; this module is what
//! QBZ calls on any official receiver (a Bluesound/NAD BluOS player, another
//! QBZ, qbzd): browse `_qobuz-connect._tcp`, read the renderer's display and
//! connect info, and hand it the credentials `/qws/delegateAuth` minted for
//! it so it joins THIS account's session — after which the cloud announces it
//! like any other renderer. Without this half a LAN-only renderer is
//! invisible to QBZ until some official app pairs it (the "it showed up once,
//! for a day" report).
//!
//! The network calls mirror the Electron controller: IPv4 addresses before
//! IPv6, 10 s per request, `Connection: close`, and HTTP 200 as the only
//! success. The pure helpers (TXT record → candidate, URL joining, address
//! order, the handoff body) are tested; nothing here validates credentials —
//! the controller only ever forwards what the cloud minted for the renderer.

use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use mdns_sd::{ResolvedService, ServiceDaemon, ServiceEvent};
use serde::Serialize;
use zeroize::Zeroize;

use crate::model::{ConnectInfo, DisplayInfo};
use crate::server::SERVICE_TYPE;

/// Electron gives each LAN request 10 s.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// The Electron controller rejects renderers announcing an older SDK.
const MIN_SDK_VERSION: (u32, u32, u32) = (0, 9, 5);

#[derive(Debug, thiserror::Error)]
pub enum LanControllerError {
    #[error("qconnect-lan-controller-mdns")]
    Mdns,
    #[error("qconnect-lan-controller-no-address")]
    NoAddress,
    #[error("qconnect-lan-controller-http: {0}")]
    Http(String),
    #[error("qconnect-lan-controller-status: {0}")]
    Status(u16),
    #[error("qconnect-lan-controller-decode: {0}")]
    Decode(String),
}

/// One `_qobuz-connect._tcp` announcement, as the official controllers read
/// it: `device_uuid` and `sdk_version` are required, `path` optional.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanRendererCandidate {
    pub device_uuid: String,
    pub sdk_version: String,
    /// The announced path with the surrounding `/` stripped ("" when absent).
    pub path: String,
    /// The mDNS instance name (what a controller shows before probing).
    pub instance: String,
    pub fullname: String,
    pub addresses: Vec<IpAddr>,
    pub port: u16,
}

impl LanRendererCandidate {
    /// Build from a resolved service. `None` when a required TXT key is
    /// missing or the SDK is older than the Electron floor.
    pub fn from_resolved(info: &ResolvedService) -> Option<Self> {
        let device_uuid = info.get_property_val_str("device_uuid")?.trim().to_string();
        let sdk_version = info.get_property_val_str("sdk_version")?.trim().to_string();
        if device_uuid.is_empty() || !sdk_version_ok(&sdk_version) {
            return None;
        }
        let path = info
            .get_property_val_str("path")
            .map(|p| p.trim().trim_matches('/').to_string())
            .unwrap_or_default();
        let fullname = info.get_fullname().to_string();
        let instance = fullname
            .strip_suffix(&format!(".{SERVICE_TYPE}"))
            .unwrap_or(&fullname)
            .to_string();
        let addresses = ordered_addresses(
            &info
                .get_addresses()
                .iter()
                .map(|a| a.to_ip_addr())
                .collect::<Vec<_>>(),
        );
        Some(Self {
            device_uuid,
            sdk_version,
            path,
            instance,
            fullname,
            addresses,
            port: info.get_port(),
        })
    }

    pub fn from_parts(
        device_uuid: &str,
        sdk_version: &str,
        path: Option<&str>,
        fullname: &str,
        addresses: &[IpAddr],
        port: u16,
    ) -> Option<Self> {
        let sdk_version = sdk_version.trim();
        if device_uuid.trim().is_empty() || !sdk_version_ok(sdk_version) {
            return None;
        }
        Some(Self {
            device_uuid: device_uuid.trim().to_string(),
            sdk_version: sdk_version.to_string(),
            path: path.map(|p| p.trim().trim_matches('/').to_string()).unwrap_or_default(),
            instance: fullname
                .strip_suffix(&format!(".{SERVICE_TYPE}"))
                .unwrap_or(fullname)
                .to_string(),
            fullname: fullname.to_string(),
            addresses: ordered_addresses(addresses),
            port,
        })
    }
}

/// `sdk_version >= 0.9.5` (Electron's floor); an unparsable version is
/// accepted, the way Android accepts any string.
fn sdk_version_ok(version: &str) -> bool {
    let mut parts = version.split('.').map(|p| p.trim().parse::<u32>());
    let (Some(Ok(major)), Some(Ok(minor)), Some(Ok(patch))) =
        (parts.next(), parts.next(), parts.next())
    else {
        return true;
    };
    (major, minor, patch) >= MIN_SDK_VERSION
}

/// IPv4 first, then IPv6 (Electron races v4, then v6 ~100 ms later, then the
/// hostname; sequential in that order gives the same first answer).
pub fn ordered_addresses(addresses: &[IpAddr]) -> Vec<IpAddr> {
    let mut seen = HashSet::new();
    let mut out: Vec<IpAddr> = addresses
        .iter()
        .copied()
        .filter(|a| a.is_ipv4() && !a.is_loopback() && seen.insert(*a))
        .collect();
    out.extend(
        addresses
            .iter()
            .copied()
            .filter(|a| a.is_ipv6() && !a.is_loopback() && seen.insert(*a)),
    );
    out
}

/// `http://{host}:{port}/{path}/{endpoint}` — IPv6 hosts bracketed, an empty
/// path collapsed (Android trims `/` around the announced path, Electron
/// concatenates it with each endpoint).
pub fn endpoint_url(address: &IpAddr, port: u16, path: &str, endpoint: &str) -> String {
    let host = match address {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => format!("[{v6}]"),
    };
    let path = path.trim_matches('/');
    let endpoint = endpoint.trim_matches('/');
    if path.is_empty() {
        format!("http://{host}:{port}/{endpoint}")
    } else {
        format!("http://{host}:{port}/{path}/{endpoint}")
    }
}

/// What the two probes told us about a candidate, plus the address that
/// answered (the handoff goes to the same one).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanRendererProbe {
    pub display: DisplayInfo,
    pub connect: ConnectInfo,
    pub address: IpAddr,
}

/// One delegated token on the LAN wire. Zeroized on drop; no `Debug`.
#[derive(Serialize)]
pub struct LanTokenOut {
    pub endpoint: String,
    pub exp: i64,
    pub jwt: String,
}

impl Drop for LanTokenOut {
    fn drop(&mut self) {
        self.endpoint.zeroize();
        self.jwt.zeroize();
    }
}

/// The official `POST connect-to-qconnect` body: the CONTROLLER's session
/// uuid, the delegated API token, the delegated QWS token under its LAN name
/// `jwt_qconnect`, and `become_active`. No `Debug`.
#[derive(Serialize)]
pub struct HandoffBody {
    pub session_id: String,
    pub jwt_api: LanTokenOut,
    pub jwt_qconnect: LanTokenOut,
    pub become_active: bool,
}

impl Drop for HandoffBody {
    fn drop(&mut self) {
        self.session_id.zeroize();
    }
}

/// Browse events, delivered on the browser's own thread.
#[derive(Debug, Clone)]
pub enum LanBrowseEvent {
    Found(LanRendererCandidate),
    /// The service's fullname, as announced when it was found.
    Lost(String),
}

/// A `_qobuz-connect._tcp` browser. `own_device_uuid` is filtered out so QBZ
/// never lists itself. Stops on drop.
pub struct LanBrowser {
    daemon: Option<ServiceDaemon>,
    running: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl LanBrowser {
    pub fn start<F>(own_device_uuid: Option<String>, on_event: F) -> Result<Self, LanControllerError>
    where
        F: Fn(LanBrowseEvent) + Send + 'static,
    {
        let daemon = ServiceDaemon::new().map_err(|_| LanControllerError::Mdns)?;
        let receiver = daemon
            .browse(SERVICE_TYPE)
            .map_err(|_| LanControllerError::Mdns)?;
        let running = Arc::new(AtomicBool::new(true));
        let thread_running = Arc::clone(&running);
        let thread = std::thread::Builder::new()
            .name("qconnect-lan-browse".to_string())
            .spawn(move || {
                let fullname_uuid: Mutex<HashMap<String, String>> = Mutex::new(HashMap::new());
                for event in receiver.iter() {
                    if !thread_running.load(Ordering::SeqCst) {
                        break;
                    }
                    match event {
                        ServiceEvent::ServiceResolved(info) => {
                            let Some(candidate) = LanRendererCandidate::from_resolved(&info)
                            else {
                                continue;
                            };
                            if own_device_uuid.as_deref() == Some(candidate.device_uuid.as_str()) {
                                continue;
                            }
                            if let Ok(mut map) = fullname_uuid.lock() {
                                map.insert(candidate.fullname.clone(), candidate.device_uuid.clone());
                            }
                            on_event(LanBrowseEvent::Found(candidate));
                        }
                        ServiceEvent::ServiceRemoved(_, fullname) => {
                            let known = fullname_uuid
                                .lock()
                                .map(|mut map| map.remove(&fullname).is_some())
                                .unwrap_or(false);
                            if known {
                                on_event(LanBrowseEvent::Lost(fullname));
                            }
                        }
                        _ => {}
                    }
                }
            })
            .map_err(|_| LanControllerError::Mdns)?;
        Ok(Self {
            daemon: Some(daemon),
            running,
            thread: Some(thread),
        })
    }

    pub fn shutdown(&mut self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(daemon) = self.daemon.take() {
            let _ = daemon.stop_browse(SERVICE_TYPE);
            let _ = daemon.shutdown();
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for LanBrowser {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// The HTTP side: probe a candidate, hand it delegated credentials.
pub struct LanControllerClient {
    http: reqwest::Client,
}

impl LanControllerClient {
    pub fn new() -> Result<Self, LanControllerError> {
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .no_proxy()
            .build()
            .map_err(|e| LanControllerError::Http(e.to_string()))?;
        Ok(Self { http })
    }

    /// `GET get-display-info` + `GET get-connect-info` on the first address
    /// that answers (IPv4 first). Both must succeed on the same address.
    pub async fn probe(
        &self,
        candidate: &LanRendererCandidate,
    ) -> Result<LanRendererProbe, LanControllerError> {
        let mut last = LanControllerError::NoAddress;
        for address in &candidate.addresses {
            let display = match self
                .get_json::<DisplayInfo>(&endpoint_url(
                    address,
                    candidate.port,
                    &candidate.path,
                    "get-display-info",
                ))
                .await
            {
                Ok(display) => display,
                Err(error) => {
                    last = error;
                    continue;
                }
            };
            let connect = match self
                .get_json::<ConnectInfo>(&endpoint_url(
                    address,
                    candidate.port,
                    &candidate.path,
                    "get-connect-info",
                ))
                .await
            {
                Ok(connect) => connect,
                Err(error) => {
                    last = error;
                    continue;
                }
            };
            return Ok(LanRendererProbe {
                display,
                connect,
                address: *address,
            });
        }
        Err(last)
    }

    /// `POST connect-to-qconnect`. Only HTTP 200 counts; the body is ignored
    /// (official controllers do not depend on one).
    pub async fn hand_off(
        &self,
        candidate: &LanRendererCandidate,
        address: &IpAddr,
        body: &HandoffBody,
    ) -> Result<(), LanControllerError> {
        let url = endpoint_url(address, candidate.port, &candidate.path, "connect-to-qconnect");
        let payload = serde_json::to_vec(body).map_err(|e| LanControllerError::Decode(e.to_string()))?;
        let response = self
            .http
            .post(&url)
            .header(reqwest::header::CONNECTION, "close")
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(payload)
            .send()
            .await
            .map_err(|e| LanControllerError::Http(e.to_string()))?;
        let status = response.status();
        if status.as_u16() != 200 {
            return Err(LanControllerError::Status(status.as_u16()));
        }
        Ok(())
    }

    async fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
    ) -> Result<T, LanControllerError> {
        let response = self
            .http
            .get(url)
            .header(reqwest::header::CONNECTION, "close")
            .send()
            .await
            .map_err(|e| LanControllerError::Http(e.to_string()))?;
        let status = response.status();
        if status.as_u16() != 200 {
            return Err(LanControllerError::Status(status.as_u16()));
        }
        let body = response
            .text()
            .await
            .map_err(|e| LanControllerError::Http(e.to_string()))?;
        serde_json::from_str(&body).map_err(|e| LanControllerError::Decode(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    fn v4(a: u8) -> IpAddr {
        IpAddr::V4(Ipv4Addr::new(192, 168, 0, a))
    }

    #[test]
    fn candidate_requires_uuid_and_sdk_and_trims_the_path() {
        let full = format!("Node.{SERVICE_TYPE}");
        let c = LanRendererCandidate::from_parts("abc", "1.2.3", Some("/api/"), &full, &[v4(9)], 8080)
            .unwrap();
        assert_eq!((c.path.as_str(), c.instance.as_str(), c.port), ("api", "Node", 8080));
        assert!(LanRendererCandidate::from_parts("", "1.2.3", None, &full, &[v4(9)], 80).is_none());
        assert!(
            LanRendererCandidate::from_parts("abc", "0.9.4", None, &full, &[v4(9)], 80).is_none(),
            "below the Electron SDK floor"
        );
        assert!(LanRendererCandidate::from_parts("abc", "0.9.5", None, &full, &[v4(9)], 80).is_some());
        assert!(
            LanRendererCandidate::from_parts("abc", "bluos", None, &full, &[v4(9)], 80).is_some(),
            "an unparsable version is accepted, like Android"
        );
    }

    #[test]
    fn addresses_go_ipv4_first_without_loopback_or_duplicates() {
        let v6 = IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1));
        let ordered = ordered_addresses(&[v6, IpAddr::V4(Ipv4Addr::LOCALHOST), v4(5), v4(5)]);
        assert_eq!(ordered, vec![v4(5), v6]);
    }

    #[test]
    fn endpoint_urls_join_like_the_official_controllers() {
        assert_eq!(
            endpoint_url(&v4(7), 8765, "", "get-display-info"),
            "http://192.168.0.7:8765/get-display-info"
        );
        assert_eq!(
            endpoint_url(&v4(7), 8765, "/qobuz/", "connect-to-qconnect"),
            "http://192.168.0.7:8765/qobuz/connect-to-qconnect"
        );
        let v6 = IpAddr::V6(Ipv6Addr::new(0xfe80, 0, 0, 0, 0, 0, 0, 1));
        assert_eq!(
            endpoint_url(&v6, 80, "", "get-connect-info"),
            "http://[fe80::1]:80/get-connect-info"
        );
    }

    #[test]
    fn handoff_body_has_exactly_the_official_four_fields() {
        let body = HandoffBody {
            session_id: "sess".into(),
            jwt_api: LanTokenOut {
                endpoint: "https://api.example".into(),
                exp: 1_780_000_000,
                jwt: "a".into(),
            },
            jwt_qconnect: LanTokenOut {
                endpoint: "wss://qws.example".into(),
                exp: 1_780_000_000,
                jwt: "q".into(),
            },
            become_active: true,
        };
        let json: serde_json::Value = serde_json::from_slice(&serde_json::to_vec(&body).unwrap()).unwrap();
        let mut keys: Vec<&str> = json.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(keys, ["become_active", "jwt_api", "jwt_qconnect", "session_id"]);
        assert_eq!(json["jwt_qconnect"]["endpoint"], "wss://qws.example");
        assert_eq!(json["jwt_api"]["exp"], 1_780_000_000);
        assert_eq!(json["become_active"], true);
    }

    #[test]
    fn unknown_enum_values_decode_to_unknown() {
        let d: DisplayInfo = serde_json::from_str(
            r#"{"friendly_name":"Node","serial_number":"1","brand_display_name":"Bluesound","model_display_name":"NODE","max_audio_quality":"UP_TO_DSD","type":"Fridge"}"#,
        )
        .unwrap();
        assert_eq!(d.max_audio_quality, crate::MaxAudioQuality::Unknown);
        assert_eq!(d.device_type, crate::DeviceType::Unknown);
        assert_eq!(d.software_version, "");
        let c: ConnectInfo = serde_json::from_str(r#"{"app_id":"x","current_session_id":null}"#).unwrap();
        assert_eq!(c.current_session_id, None);
    }
}
