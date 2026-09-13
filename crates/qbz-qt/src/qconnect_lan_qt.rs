//! Thin Qt adapter for the official QConnect LAN receiver surface.
//!
//! This module projects Qt's existing QConnect identity into the LAN wire,
//! owns the listener/mDNS lifetime, and drains the bounded latest-wins
//! admission slot on one dedicated thread. Cloud validation and transactional
//! authority switching deliberately remain outside this adapter.

use std::collections::HashMap;
use std::error::Error;
use std::fmt;
use std::future::Future;
use std::sync::{Arc, Mutex as StdMutex};
use std::thread::JoinHandle;
use std::time::Duration;

use qbz_models::Quality;
use qconnect_app::LanProjectionSlot;
use qconnect_lan::{
    ConnectInfo, DeviceType, DisplayInfo, EndpointPolicy, HandoffCandidate, LanBrowseEvent,
    LanBrowser, LanControllerClient, LanError, LanProjection, LanRendererCandidate,
    LanRendererProbe, LanService, LanServiceConfig, MaxAudioQuality,
};

use crate::qconnect_transport_qt::default_qconnect_device_info;

const ADMISSION_WAIT: Duration = Duration::from_secs(60);
const QT_MODEL: &str = "QBZ Desktop";
const ADMISSION_THREAD_NAME: &str = "qbz-qt-qconnect-lan-admission";

pub(crate) type QtLanProjectionSlot = LanProjectionSlot;

/// Map the effective local playback ceiling to the exact official LAN enum.
/// The same `local_playback_quality` source is used by stream resolution and
/// cloud renderer reports; LAN must not advertise an independent capability.
pub const fn max_audio_quality_for_quality(quality: Quality) -> MaxAudioQuality {
    match quality {
        Quality::Mp3 => MaxAudioQuality::MP3,
        Quality::Lossless => MaxAudioQuality::UpToCd,
        Quality::HiRes => MaxAudioQuality::UpToHires96,
        Quality::UltraHiRes => MaxAudioQuality::UpToHires192,
    }
}

#[derive(Debug)]
pub enum QtLanError {
    ExistingIdentityUnavailable,
    Service(LanError),
    AdmissionThread(std::io::Error),
}

impl fmt::Display for QtLanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ExistingIdentityUnavailable => {
                formatter.write_str("qconnect LAN identity is unavailable")
            }
            Self::Service(error) => write!(formatter, "qconnect LAN service failed: {error}"),
            Self::AdmissionThread(_) => {
                formatter.write_str("qconnect LAN admission thread could not start")
            }
        }
    }
}

impl Error for QtLanError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Service(error) => Some(error),
            Self::AdmissionThread(error) => Some(error),
            Self::ExistingIdentityUnavailable => None,
        }
    }
}

impl From<LanError> for QtLanError {
    fn from(error: LanError) -> Self {
        Self::Service(error)
    }
}

struct ExistingIdentity {
    device_uuid: String,
    friendly_name: String,
    brand: String,
    software_version: String,
}

impl ExistingIdentity {
    fn resolve() -> Result<Self, QtLanError> {
        let device_info = default_qconnect_device_info();
        let (Some(device_uuid), Some(friendly_name), Some(brand), Some(software_version)) = (
            device_info.device_uuid,
            device_info.friendly_name,
            device_info.brand,
            device_info.software_version,
        ) else {
            return Err(QtLanError::ExistingIdentityUnavailable);
        };
        if device_uuid.trim().is_empty()
            || friendly_name.trim().is_empty()
            || brand.trim().is_empty()
            || software_version.trim().is_empty()
        {
            return Err(QtLanError::ExistingIdentityUnavailable);
        }
        Ok(Self {
            device_uuid,
            friendly_name,
            brand,
            software_version,
        })
    }
}

fn build_projection(
    identity: &ExistingIdentity,
    app_id: String,
    max_audio_quality: MaxAudioQuality,
    current_session_id: Option<String>,
) -> LanProjection {
    LanProjection::new(
        DisplayInfo {
            friendly_name: identity.friendly_name.clone(),
            serial_number: identity.device_uuid.clone(),
            brand_display_name: identity.brand.clone(),
            model_display_name: QT_MODEL.to_string(),
            max_audio_quality,
            device_type: DeviceType::Computer,
            software_version: identity.software_version.clone(),
        },
        ConnectInfo {
            app_id,
            current_session_id,
        },
    )
}

/// Owns the Qt LAN service and its blocking admission bridge.
///
/// The callback future is driven on `runtime_handle` while the dedicated
/// bridge thread waits for it. Consequently, candidates are admitted in the
/// exact order observed from the latest-wins slot without blocking a Tokio
/// worker or the Qt UI thread. The callback must provide its own bounded and
/// cancellation-safe transaction semantics.
/// A renderer seen on this network: the row the controller half adds to the
/// picker (`lan: true`) until the cloud lists the device itself.
#[derive(Debug, Clone)]
pub(crate) struct LanPickerRow {
    pub device_uuid: String,
    pub name: String,
    pub brand: String,
    pub model: String,
    pub device_type: DeviceType,
}

/// What the browser found and what the probe learned, by device uuid.
#[derive(Default)]
struct LanCandidates {
    found: HashMap<String, LanRendererCandidate>,
    probed: HashMap<String, LanRendererProbe>,
    fullname_uuid: HashMap<String, String>,
}

impl LanCandidates {
    fn rows(&self) -> Vec<LanPickerRow> {
        let mut rows: Vec<LanPickerRow> = self
            .probed
            .iter()
            .map(|(uuid, probe)| LanPickerRow {
                device_uuid: uuid.clone(),
                name: probe.display.friendly_name.clone(),
                brand: probe.display.brand_display_name.clone(),
                model: probe.display.model_display_name.clone(),
                device_type: probe.display.device_type,
            })
            .collect();
        rows.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        rows
    }
}

fn publish_lan_rows(candidates: &StdMutex<LanCandidates>) {
    let rows = candidates.lock().map(|c| c.rows()).unwrap_or_default();
    crate::qconnect_qt::publish::set_lan_rows(rows);
}

pub struct QtLanRuntime {
    service: Option<LanService>,
    projection: LanProjection,
    admission_bridge: Option<JoinHandle<()>>,
    /// The controller half (2026-09-13): browse + probe, same lifetime as
    /// the receiver above. `None` when the browser could not start — the
    /// receiver keeps working without it.
    browser: Option<LanBrowser>,
    candidates: Arc<StdMutex<LanCandidates>>,
    client: Option<Arc<LanControllerClient>>,
}

impl QtLanRuntime {
    pub fn start<F, Fut>(
        runtime_handle: tokio::runtime::Handle,
        endpoint_policy: EndpointPolicy,
        app_id: String,
        max_audio_quality: MaxAudioQuality,
        current_session_id: Option<String>,
        on_handoff: F,
    ) -> Result<Self, QtLanError>
    where
        F: Fn(HandoffCandidate) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        let identity = ExistingIdentity::resolve()?;
        let own_device_uuid = identity.device_uuid.clone();
        let projection = build_projection(&identity, app_id, max_audio_quality, current_session_id);
        let config =
            LanServiceConfig::new(projection.clone(), endpoint_policy, identity.device_uuid);
        let (mut service, inbox) = LanService::start(config)?;
        let on_handoff = Arc::new(on_handoff);
        let browse_handle = runtime_handle.clone();

        let admission_bridge = match std::thread::Builder::new()
            .name(ADMISSION_THREAD_NAME.to_string())
            .spawn(move || loop {
                match inbox.take_timeout(ADMISSION_WAIT) {
                    Some(candidate) => {
                        let callback = Arc::clone(&on_handoff);
                        runtime_handle.block_on(callback(candidate));
                    }
                    None if inbox.is_closed() => break,
                    None => {}
                }
            }) {
            Ok(thread) => thread,
            Err(error) => {
                service.shutdown();
                return Err(QtLanError::AdmissionThread(error));
            }
        };

        // The controller half: browse `_qobuz-connect._tcp`, probe every
        // announcement that is not us, and offer what answers as picker rows.
        // A failure here only costs the LAN candidates; the receiver stays.
        let candidates: Arc<StdMutex<LanCandidates>> = Arc::default();
        let client = match LanControllerClient::new() {
            Ok(client) => Some(Arc::new(client)),
            Err(error) => {
                log::warn!("[QConnect LAN] controller client unavailable: {error}");
                None
            }
        };
        let browser = client.as_ref().and_then(|client| {
            let client = Arc::clone(client);
            let candidates = Arc::clone(&candidates);
            match LanBrowser::start(Some(own_device_uuid), move |event| match event {
                LanBrowseEvent::Found(candidate) => {
                    if let Ok(mut c) = candidates.lock() {
                        c.fullname_uuid
                            .insert(candidate.fullname.clone(), candidate.device_uuid.clone());
                        c.found
                            .insert(candidate.device_uuid.clone(), candidate.clone());
                    }
                    let client = Arc::clone(&client);
                    let candidates = Arc::clone(&candidates);
                    browse_handle.spawn(async move {
                        match client.probe(&candidate).await {
                            Ok(probe) => {
                                log::info!(
                                    "[QConnect LAN] on this network: {} ({} {}) at {}",
                                    probe.display.friendly_name,
                                    probe.display.brand_display_name,
                                    probe.display.model_display_name,
                                    probe.address
                                );
                                if let Ok(mut c) = candidates.lock() {
                                    c.probed.insert(candidate.device_uuid.clone(), probe);
                                }
                                publish_lan_rows(&candidates);
                            }
                            Err(error) => log::info!(
                                "[QConnect LAN] {} did not answer the probe: {error}",
                                candidate.instance
                            ),
                        }
                    });
                }
                LanBrowseEvent::Lost(fullname) => {
                    if let Ok(mut c) = candidates.lock() {
                        if let Some(uuid) = c.fullname_uuid.remove(&fullname) {
                            c.found.remove(&uuid);
                            c.probed.remove(&uuid);
                        }
                    }
                    publish_lan_rows(&candidates);
                }
            }) {
                Ok(browser) => Some(browser),
                Err(error) => {
                    log::warn!("[QConnect LAN] browse unavailable: {error}");
                    None
                }
            }
        });

        Ok(Self {
            service: Some(service),
            projection,
            admission_bridge: Some(admission_bridge),
            browser,
            candidates,
            client,
        })
    }

    pub fn projection(&self) -> LanProjection {
        self.projection.clone()
    }

    /// A probed candidate by device uuid, with the HTTP client to reach it.
    pub(crate) fn lan_candidate(
        &self,
        device_uuid: &str,
    ) -> Option<(LanRendererCandidate, LanRendererProbe, Arc<LanControllerClient>)> {
        let client = Arc::clone(self.client.as_ref()?);
        let c = self.candidates.lock().ok()?;
        let candidate = c.found.get(device_uuid)?.clone();
        let probe = c.probed.get(device_uuid)?.clone();
        Some((candidate, probe, client))
    }

    pub fn port(&self) -> Option<u16> {
        self.service.as_ref().map(LanService::port)
    }

    /// Idempotent blocking teardown in the normative order: withdraw
    /// discovery and stop HTTP admission first, then wait for the now-woken
    /// bridge to exit. Callers in async or UI contexts must run this method on
    /// a blocking worker.
    pub fn shutdown_blocking(&mut self) {
        if let Some(mut browser) = self.browser.take() {
            browser.shutdown();
            if let Ok(mut c) = self.candidates.lock() {
                *c = LanCandidates::default();
            }
            crate::qconnect_qt::publish::set_lan_rows(Vec::new());
        }
        if let Some(mut service) = self.service.take() {
            service.shutdown();
        }
        if let Some(bridge) = self.admission_bridge.take() {
            let _ = bridge.join();
        }
    }
}

impl Drop for QtLanRuntime {
    fn drop(&mut self) {
        self.shutdown_blocking();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_identity() -> ExistingIdentity {
        ExistingIdentity {
            device_uuid: "de0c0d70-0000-4000-8000-000000000001".to_string(),
            friendly_name: "Desktop probe".to_string(),
            brand: "QBZ".to_string(),
            software_version: "qbz/2.1.0".to_string(),
        }
    }

    #[test]
    fn desktop_projection_uses_existing_identity_and_official_role() {
        let projection = build_projection(
            &fixture_identity(),
            "app-id".to_string(),
            MaxAudioQuality::UpToHires192,
            None,
        );

        assert_eq!(
            projection.display_info(),
            DisplayInfo {
                friendly_name: "Desktop probe".to_string(),
                serial_number: "de0c0d70-0000-4000-8000-000000000001".to_string(),
                brand_display_name: "QBZ".to_string(),
                model_display_name: "QBZ Desktop".to_string(),
                max_audio_quality: MaxAudioQuality::UpToHires192,
                device_type: DeviceType::Computer,
                software_version: "qbz/2.1.0".to_string(),
            }
        );
        assert_eq!(
            projection.connect_info(),
            ConnectInfo {
                app_id: "app-id".to_string(),
                current_session_id: None,
            }
        );
    }

    #[test]
    fn quality_mapping_matches_official_lan_families() {
        assert_eq!(
            max_audio_quality_for_quality(Quality::Mp3),
            MaxAudioQuality::MP3
        );
        assert_eq!(
            max_audio_quality_for_quality(Quality::Lossless),
            MaxAudioQuality::UpToCd
        );
        assert_eq!(
            max_audio_quality_for_quality(Quality::HiRes),
            MaxAudioQuality::UpToHires96
        );
        assert_eq!(
            max_audio_quality_for_quality(Quality::UltraHiRes),
            MaxAudioQuality::UpToHires192
        );
    }

    #[test]
    fn projection_session_id_changes_only_when_explicitly_updated() {
        let projection = build_projection(
            &fixture_identity(),
            "app-id".to_string(),
            MaxAudioQuality::UpToCd,
            None,
        );
        assert_eq!(projection.connect_info().current_session_id, None);

        projection.set_current_session_id(Some("delegated-session".to_string()));
        assert_eq!(
            projection.connect_info().current_session_id.as_deref(),
            Some("delegated-session")
        );
        projection.set_current_session_id(None);
        assert_eq!(projection.connect_info().current_session_id, None);
    }
}
