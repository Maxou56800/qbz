// crates/qbzd/src/tui/screens/audio.rs — the Audio screen (03-setup-tui.md §3.2).
//
// The J1-critical screen. Writes audio_settings.db in the daemon data root via
// AudioSettingsStore::new_at (reused through the App's write_one path — no new
// persistence). The three load-bearing PURE pieces (unit-tested at the bottom):
//   1. the constraint matrix (§3.2.3 shown/enabled) — `row_state`;
//   2. the cross-setting cascades (§3.2.3 items 1-7) — `cascade_*`;
//   3. the device picker grouping (§3.2.2), re-derived from the desktop
//      `crates/qbz/src/settings.rs` rules (we must NOT depend on the qbz bin
//      crate — it pulls qbz-ui). `group_devices` reproduces `alsa_section` /
//      `device_is_bit_perfect` / `group_alsa_devices` 1:1, including the
//      is_default-vs-section badge edge case.

use std::collections::HashMap;

use qbz_audio::alsa_hardware_volume::{
    AlsaMixerControlId, HardwareVolumeCandidate, HardwareVolumeChoiceReason,
    HardwareVolumeDecision, HardwareVolumeProbe, HardwareVolumeSelectionSource,
    HardwareVolumeUnsupportedReason, StableAlsaRouteKey,
};
use qbz_audio::settings::AudioSettings;
use qbz_audio::{AlsaPlugin, AudioBackendType, AudioDevice, BackendManager};
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::text::Line;
use ratatui::Frame;

use crate::tui::app::{DrawCtx, ScreenAction};
use crate::tui::strings as s;
use crate::tui::theme;
use crate::tui::widgets::{self, SelectOutcome, SelectPopup};

// ============================ staged form ============================

#[derive(Debug, Clone, PartialEq)]
pub struct StagedAudio {
    pub backend: AudioBackendType,
    pub output_device: Option<String>,
    pub alsa_plugin: AlsaPlugin,
    pub alsa_hardware_volume: bool,
    pub alsa_hardware_volume_control: Option<AlsaMixerControlId>,
    pub alsa_hardware_volume_controls: HashMap<StableAlsaRouteKey, AlsaMixerControlId>,
    pub dsd_mode: String,
    pub exclusive_mode: bool,
    pub reserve_dac: bool,
    pub dac_passthrough: bool,
    pub pw_force_bitperfect: bool,
    pub skip_sink_switch: bool,
    pub stream_first_track: bool,
    pub stream_buffer_seconds: u8,
    pub streaming_only: bool,
    /// Carried (not shown here) so the §3.2.3 cascades that force gapless off
    /// (backend=ALSA, streaming-only=ON) persist through the Audio save.
    pub gapless_enabled: bool,
}

impl StagedAudio {
    pub fn from_settings(a: &AudioSettings) -> Self {
        let alsa_hardware_volume_control = a
            .output_device
            .as_deref()
            .and_then(|device| qbz_audio::alsa_hardware_volume::stable_alsa_route_key(device).ok())
            .and_then(|route| a.alsa_hardware_volume_controls.get(&route).cloned());
        Self {
            backend: a.backend_type.unwrap_or_default(),
            output_device: a.output_device.clone(),
            alsa_plugin: a.alsa_plugin.unwrap_or_default(),
            alsa_hardware_volume: a.alsa_hardware_volume,
            alsa_hardware_volume_control,
            alsa_hardware_volume_controls: a.alsa_hardware_volume_controls.clone(),
            dsd_mode: a.dsd_mode.clone(),
            exclusive_mode: a.exclusive_mode,
            reserve_dac: a.reserve_dac_while_running,
            dac_passthrough: a.dac_passthrough,
            pw_force_bitperfect: a.pw_force_bitperfect,
            skip_sink_switch: a.skip_sink_switch,
            stream_first_track: a.stream_first_track,
            stream_buffer_seconds: a.stream_buffer_seconds,
            streaming_only: a.streaming_only,
            gapless_enabled: a.gapless_enabled,
        }
    }
}

// ============================ fields + constraint matrix ============================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AField {
    Backend,
    Device,
    AlsaPlugin,
    HwVolume,
    HwVolumeControl,
    Dsd,
    Exclusive,
    Reserve,
    Passthrough,
    ForceBp,
    LockOutput,
    StreamUncached,
    Buffer,
    StreamingOnly,
}

/// Constraint matrix (§3.2.3): `(shown, enabled, disabled_reason)`.
pub fn row_state(field: AField, a: &StagedAudio) -> (bool, bool, Option<&'static str>) {
    use AField::*;
    let alsa = a.backend == AudioBackendType::Alsa;
    let pipewire = a.backend == AudioBackendType::PipeWire;
    match field {
        Backend => (true, true, None),
        Device => (true, true, None),
        AlsaPlugin => (alsa, true, None), // shown only on ALSA
        // NB: `use AField::*` shadows the AlsaPlugin type here — qualify it.
        HwVolume => (
            alsa && a.alsa_plugin != qbz_audio::AlsaPlugin::Pcm,
            true,
            None,
        ),
        HwVolumeControl => (
            alsa && a.alsa_plugin != qbz_audio::AlsaPlugin::Pcm
                && a.alsa_hardware_volume
                && a.alsa_hardware_volume_control.is_some(),
            true,
            None,
        ),
        Dsd => (alsa, true, None),
        Exclusive => (true, alsa, if alsa { None } else { Some(s::R_ALSA_ONLY) }),
        Reserve => (true, true, None),
        Passthrough => (
            true,
            pipewire,
            if pipewire {
                None
            } else {
                Some(s::R_PIPEWIRE_ONLY)
            },
        ),
        // shown only when passthrough on AND PipeWire.
        ForceBp => (a.dac_passthrough && pipewire, true, None),
        // shown when PipeWire; enabled when passthrough OFF.
        LockOutput => (
            pipewire,
            !a.dac_passthrough,
            if a.dac_passthrough {
                Some(s::R_PASSTHROUGH_OFF)
            } else {
                None
            },
        ),
        StreamUncached => (true, true, None),
        Buffer => (a.stream_first_track, true, None), // shown when stream uncached on
        StreamingOnly => (true, true, None),
    }
}

/// The fields currently SHOWN, top-to-bottom (focus navigates this list).
pub fn visible_fields(a: &StagedAudio) -> Vec<AField> {
    use AField::*;
    [
        Backend,
        Device,
        AlsaPlugin,
        HwVolume,
        HwVolumeControl,
        Dsd,
        Exclusive,
        Reserve,
        Passthrough,
        ForceBp,
        LockOutput,
        StreamUncached,
        Buffer,
        StreamingOnly,
    ]
    .into_iter()
    .filter(|f| row_state(*f, a).0)
    .collect()
}

// ============================ cascades (§3.2.3) ============================

/// Toggle cascades (items 1-3), fired the moment a toggle flips.
pub fn cascade_on_toggle(a: &mut StagedAudio, field: AField) {
    match field {
        AField::Passthrough => {
            if a.dac_passthrough {
                a.skip_sink_switch = false; // item 1: mutually exclusive
            } else {
                a.pw_force_bitperfect = false; // item 2
            }
        }
        _ => {}
    }
}

/// Backend-switch cascades (items 4-7), fired when Backend changes. The device
/// reset (item 7) means the caller must re-enumerate for the new backend.
pub fn cascade_on_backend_change(a: &mut StagedAudio) {
    cascade_on_backend_change_for(a, cfg!(target_os = "macos"))
}

/// `cascade_on_backend_change` with the platform as a parameter so the macOS
/// arm of item 5 runs on the Linux gate.
pub fn cascade_on_backend_change_for(a: &mut StagedAudio, macos: bool) {
    if a.backend != AudioBackendType::PipeWire {
        a.dac_passthrough = false; // item 4
        a.pw_force_bitperfect = false;
    }
    // Item 5: exclusive survives only onto a backend that honours it — ALSA,
    // and on macOS System default, whose `exclusive_mode` is CoreAudio Hog
    // Mode (PR #391). #748 parity with the Qt shell's cascade
    // (`settings_qt::backend_switch_clears_exclusive`).
    let honours_exclusive = a.backend == AudioBackendType::Alsa
        || (macos && a.backend == AudioBackendType::SystemDefault);
    if !honours_exclusive {
        a.exclusive_mode = false;
    }
    if a.backend == AudioBackendType::Alsa {
        a.gapless_enabled = false; // item 6
    }
    a.output_device = None; // item 7: never carry the old backend's device id
    a.alsa_hardware_volume = false;
    a.alsa_hardware_volume_control = None;
}

// ============================ device picker grouping (§3.2.2) ============================

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum AlsaSection {
    Defaults,
    BitPerfect,
    PluginHw,
    Other,
}

/// 1:1 with desktop `alsa_section` (`crates/qbz/src/settings.rs:286-301`).
fn alsa_section(id: &str, is_default: bool, label: &str) -> AlsaSection {
    let id_l = id.to_ascii_lowercase();
    if id.is_empty() || id_l == "default" || is_default {
        AlsaSection::Defaults
    } else if id_l.starts_with("hw:")
        || id_l.starts_with("iec958:")
        || id_l.starts_with("front:card=")
        || label.to_ascii_lowercase().contains("bit-perfect")
    {
        AlsaSection::BitPerfect
    } else if id_l.starts_with("plughw:") {
        AlsaSection::PluginHw
    } else {
        AlsaSection::Other
    }
}

fn alsa_section_label(section: AlsaSection) -> &'static str {
    match section {
        AlsaSection::Defaults => "Defaults",
        AlsaSection::BitPerfect => "Bit-perfect (Hardware / Digital)",
        AlsaSection::PluginHw => "Plugin Hardware",
        AlsaSection::Other => "Other Outputs",
    }
}

/// 1:1 with desktop `device_is_bit_perfect` (`settings.rs:323-333`). The badge
/// uses the REAL `is_default` flag — unlike the grouping call (§3.2.2 edge case).
fn device_is_bit_perfect(backend: AudioBackendType, d: &AudioDevice) -> bool {
    match backend {
        AudioBackendType::Alsa => {
            let label = d.description.as_deref().unwrap_or(&d.name);
            alsa_section(&d.id, d.is_default, label) == AlsaSection::BitPerfect
        }
        AudioBackendType::PipeWire => d.is_hardware,
        AudioBackendType::Pulse
        | AudioBackendType::SystemDefault
        | AudioBackendType::Jack
        | AudioBackendType::WasapiExclusive => false,
    }
}

/// One grouped, badged device row for the picker.
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceEntry {
    pub label: String,
    pub id: String,
    pub bp: bool,
    /// Section header shown ABOVE this row (ALSA only, first row of a section).
    pub header: Option<String>,
}

/// Re-derive the picker rows (§3.2.2): a synthetic "System default" always
/// leads; ALSA regroups into the four sections (grouping passes is_default=false
/// like `group_alsa_devices:415`); non-ALSA stays flat, no headers.
pub fn group_devices(backend: AudioBackendType, devices: Vec<AudioDevice>) -> Vec<DeviceEntry> {
    // Build rows: System default first (empty id, never BP), then devices.
    let mut rows: Vec<DeviceEntry> = vec![DeviceEntry {
        label: "System default".to_string(),
        id: String::new(),
        bp: false,
        header: None,
    }];
    for d in &devices {
        let label = match d.description.as_deref() {
            Some(desc) if !desc.is_empty() => desc.to_string(),
            _ => d.name.clone(),
        };
        rows.push(DeviceEntry {
            label,
            id: d.id.clone(),
            bp: device_is_bit_perfect(backend, d),
            header: None,
        });
    }

    if backend != AudioBackendType::Alsa {
        return rows; // flat, no headers
    }

    // ALSA: stable-sort by section (grouping ALWAYS passes is_default=false,
    // desktop `group_alsa_devices:415`), then assign a header to each section's
    // first row.
    let mut indexed: Vec<(AlsaSection, DeviceEntry)> = rows
        .into_iter()
        .map(|r| (alsa_section(&r.id, false, &r.label), r))
        .collect();
    indexed.sort_by_key(|(section, _)| *section);

    let mut out = Vec::with_capacity(indexed.len());
    let mut prev: Option<AlsaSection> = None;
    for (section, mut row) in indexed {
        if prev != Some(section) {
            prev = Some(section);
            row.header = Some(alsa_section_label(section).to_string());
        }
        out.push(row);
    }
    out
}

// ============================ screen state ============================

enum Editor {
    Backend(SelectPopup),
    Device(SelectPopup),
    AlsaPlugin(SelectPopup),
    HardwareVolume(SelectPopup),
    Dsd(SelectPopup),
    /// The §3.2.4 DSD guard: `prev` is restored on Esc.
    DsdConfirm {
        new: String,
        prev: String,
    },
}

pub struct AudioState {
    baseline: StagedAudio,
    staged: StagedAudio,
    focus: usize,
    devices: Vec<DeviceEntry>,
    scanning: bool,
    hardware_volume_candidates: Vec<HardwareVolumeCandidate>,
    hardware_volume_route_key: Option<StableAlsaRouteKey>,
    hardware_volume_status: Option<String>,
    hardware_volume_probing: bool,
    probe_for_control_change: bool,
    editor: Option<Editor>,
}

impl AudioState {
    pub fn new(settings: &AudioSettings) -> Self {
        let staged = StagedAudio::from_settings(settings);
        Self {
            baseline: staged.clone(),
            staged,
            focus: 0,
            devices: Vec::new(),
            scanning: true,
            hardware_volume_candidates: Vec::new(),
            hardware_volume_route_key: None,
            hardware_volume_status: None,
            hardware_volume_probing: false,
            probe_for_control_change: false,
            editor: None,
        }
    }

    /// The backend the App should (re-)enumerate devices for.
    pub fn backend(&self) -> AudioBackendType {
        self.staged.backend
    }

    /// Receive a device-enumeration result from the worker (§5.5).
    pub fn set_devices(&mut self, result: Result<Vec<AudioDevice>, String>) {
        self.scanning = false;
        self.devices = match result {
            Ok(list) => group_devices(self.staged.backend, list),
            Err(_) => group_devices(self.staged.backend, Vec::new()),
        };
    }

    pub fn start_scan(&mut self) {
        self.scanning = true;
    }

    pub fn hardware_volume_probe_request(
        &self,
    ) -> Result<(String, HashMap<StableAlsaRouteKey, AlsaMixerControlId>), String> {
        if self.staged.backend != AudioBackendType::Alsa
            || self.staged.alsa_plugin == AlsaPlugin::Pcm
        {
            return Err("hardware volume requires ALSA Direct (hw or plughw)".to_string());
        }
        let device_id = self
            .staged
            .output_device
            .clone()
            .ok_or_else(|| "select an explicit ALSA Direct device first".to_string())?;
        if !qbz_audio::alsa_direct::AlsaDirectStream::is_hw_device(&device_id) {
            return Err("the selected output is not an ALSA Direct route".to_string());
        }
        Ok((device_id, self.staged.alsa_hardware_volume_controls.clone()))
    }

    pub fn begin_hardware_volume_probe(&mut self, for_control_change: bool) {
        self.hardware_volume_probing = true;
        self.hardware_volume_status = Some("checking ALSA mixer controls…".to_string());
        self.probe_for_control_change = for_control_change;
    }

    pub fn set_hardware_volume_probe(
        &mut self,
        result: Result<(HardwareVolumeProbe, HardwareVolumeDecision), String>,
    ) {
        self.hardware_volume_probing = false;
        let (probe, decision) = match result {
            Ok(result) => result,
            Err(error) => {
                self.staged.alsa_hardware_volume = false;
                self.hardware_volume_status = Some(error);
                self.probe_for_control_change = false;
                return;
            }
        };
        self.hardware_volume_candidates = probe.valid_candidates().cloned().collect();
        self.hardware_volume_route_key = probe.route_key.clone();
        match decision {
            HardwareVolumeDecision::Selected { selection } => {
                let change =
                    self.probe_for_control_change && self.hardware_volume_candidates.len() > 1;
                self.staged
                    .alsa_hardware_volume_controls
                    .insert(selection.route_key, selection.control.clone());
                self.staged.alsa_hardware_volume_control = Some(selection.control);
                self.staged.alsa_hardware_volume = true;
                self.hardware_volume_status = Some(match selection.source {
                    HardwareVolumeSelectionSource::Persisted => {
                        "saved mixer control validated".to_string()
                    }
                    HardwareVolumeSelectionSource::Ucm => {
                        "mixer control resolved from ALSA UCM".to_string()
                    }
                    HardwareVolumeSelectionSource::OnlyCandidate => {
                        "only compatible mixer control selected".to_string()
                    }
                    HardwareVolumeSelectionSource::UserChoice => {
                        "mixer control selected".to_string()
                    }
                });
                if change {
                    self.open_hardware_volume_picker();
                }
            }
            HardwareVolumeDecision::NeedsChoice {
                route_key: _,
                reason,
                ..
            } => {
                self.staged.alsa_hardware_volume = false;
                self.hardware_volume_status = Some(match reason {
                    HardwareVolumeChoiceReason::PersistedSelectionStale => {
                        "saved control is unavailable — choose another".to_string()
                    }
                    HardwareVolumeChoiceReason::Ambiguous
                    | HardwareVolumeChoiceReason::UcmAmbiguous => {
                        "multiple playback controls — choose one".to_string()
                    }
                });
                self.open_hardware_volume_picker();
            }
            HardwareVolumeDecision::Unsupported { reason, .. } => {
                self.staged.alsa_hardware_volume = false;
                self.hardware_volume_status = Some(match reason {
                    HardwareVolumeUnsupportedReason::Probe(error) => error.message,
                    HardwareVolumeUnsupportedReason::NoValidControls => {
                        "no compatible playback-volume control".to_string()
                    }
                    HardwareVolumeUnsupportedReason::PersistedSelectionStale(control) => {
                        format!("saved mixer control '{control}' is unavailable")
                    }
                });
            }
        }
        self.probe_for_control_change = false;
    }

    pub fn is_dirty(&self) -> bool {
        self.staged != self.baseline
    }

    pub fn is_editing(&self) -> bool {
        self.editor.is_some()
    }

    /// The breadcrumb's level-2 node when a field editor/picker is active (the
    /// DSD guard counts — it is still editing the DSD field).
    pub fn editing_label(&self) -> Option<&'static str> {
        match &self.editor {
            Some(Editor::Backend(_)) => Some(s::A_BACKEND),
            Some(Editor::Device(_)) => Some(s::A_DEVICE),
            Some(Editor::AlsaPlugin(_)) => Some(s::A_ALSA_PLUGIN),
            Some(Editor::HardwareVolume(_)) => Some(s::A_HW_VOLUME_CONTROL),
            Some(Editor::Dsd(_)) | Some(Editor::DsdConfirm { .. }) => Some(s::A_DSD),
            None => None,
        }
    }

    /// True when the focused (non-editing) field consumes ←/→ (the Buffer
    /// slider). The shell asks this before letting ← drop focus back to the nav.
    pub fn focused_is_buffer(&self) -> bool {
        if self.editor.is_some() {
            return false;
        }
        let fields = visible_fields(&self.staged);
        fields.get(self.focus).copied() == Some(AField::Buffer)
    }

    /// Changed dotted `audio.*` keys for the save path (write_one values).
    pub fn save_keys(&self) -> Vec<(String, String)> {
        let b = &self.baseline;
        let a = &self.staged;
        let mut out = Vec::new();
        let mut push = |k: &str, v: String| out.push((format!("audio.{k}"), v));
        if a.backend != b.backend {
            push("backend", backend_value(a.backend).to_string());
        }
        if a.output_device != b.output_device {
            push(
                "device",
                a.output_device
                    .clone()
                    .unwrap_or_else(|| "system".to_string()),
            );
        }
        if a.alsa_plugin != b.alsa_plugin {
            push("alsa_plugin", alsa_plugin_value(a.alsa_plugin).to_string());
        }
        if a.alsa_hardware_volume_control != b.alsa_hardware_volume_control
            && a.alsa_hardware_volume_control.is_some()
        {
            push(
                "alsa_hardware_volume_control",
                a.alsa_hardware_volume_control
                    .as_ref()
                    .map(ToString::to_string)
                    .expect("checked above"),
            );
        }
        if a.alsa_hardware_volume != b.alsa_hardware_volume {
            push("alsa_hardware_volume", a.alsa_hardware_volume.to_string());
        }
        if a.dsd_mode != b.dsd_mode {
            push("dsd_mode", a.dsd_mode.clone());
        }
        if a.exclusive_mode != b.exclusive_mode {
            push("exclusive_mode", a.exclusive_mode.to_string());
        }
        if a.reserve_dac != b.reserve_dac {
            push("reserve_dac_while_running", a.reserve_dac.to_string());
        }
        if a.dac_passthrough != b.dac_passthrough {
            push("dac_passthrough", a.dac_passthrough.to_string());
        }
        if a.pw_force_bitperfect != b.pw_force_bitperfect {
            push("pw_force_bitperfect", a.pw_force_bitperfect.to_string());
        }
        if a.skip_sink_switch != b.skip_sink_switch {
            push("skip_sink_switch", a.skip_sink_switch.to_string());
        }
        if a.stream_first_track != b.stream_first_track {
            push("stream_first_track", a.stream_first_track.to_string());
        }
        if a.stream_buffer_seconds != b.stream_buffer_seconds {
            push("stream_buffer_seconds", a.stream_buffer_seconds.to_string());
        }
        if a.streaming_only != b.streaming_only {
            push("streaming_only", a.streaming_only.to_string());
        }
        if a.gapless_enabled != b.gapless_enabled {
            push("gapless_enabled", a.gapless_enabled.to_string());
        }
        out
    }

    pub fn mark_saved(&mut self) {
        self.baseline = self.staged.clone();
    }

    // -------------------------- input --------------------------

    pub fn handle_key(&mut self, key: KeyEvent) -> ScreenAction {
        if self.editor.is_some() {
            return self.handle_editor_key(key);
        }
        let fields = visible_fields(&self.staged);
        if self.focus >= fields.len() {
            self.focus = fields.len().saturating_sub(1);
        }
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                if self.focus == 0 {
                    self.focus = fields.len().saturating_sub(1);
                } else {
                    self.focus -= 1;
                }
                ScreenAction::Consumed
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::Tab => {
                if !fields.is_empty() {
                    self.focus = (self.focus + 1) % fields.len();
                }
                ScreenAction::Consumed
            }
            KeyCode::BackTab => {
                if self.focus == 0 {
                    self.focus = fields.len().saturating_sub(1);
                } else {
                    self.focus -= 1;
                }
                ScreenAction::Consumed
            }
            KeyCode::Char('s') => ScreenAction::Save,
            KeyCode::Char('r') => {
                self.scanning = true;
                ScreenAction::RefreshDevices
            }
            KeyCode::Char('/') => {
                if fields.get(self.focus) == Some(&AField::Device) {
                    self.open_device_picker(true);
                }
                ScreenAction::Consumed
            }
            KeyCode::Left | KeyCode::Right => {
                if fields.get(self.focus) == Some(&AField::Buffer) {
                    let d: i8 = if key.code == KeyCode::Left { -1 } else { 1 };
                    let next = (self.staged.stream_buffer_seconds as i8 + d).clamp(1, 10);
                    self.staged.stream_buffer_seconds = next as u8;
                }
                ScreenAction::Consumed
            }
            KeyCode::Enter | KeyCode::Char(' ') => {
                let field = fields.get(self.focus).copied();
                if let Some(f) = field {
                    return self.activate(f);
                }
                ScreenAction::Consumed
            }
            KeyCode::Esc => ScreenAction::Back,
            _ => ScreenAction::Consumed,
        }
    }

    /// Act on the focused field (toggle in place / open a popup). Any device
    /// re-enumeration triggered by a backend change is returned from
    /// `handle_editor_key` when the picker resolves, not here.
    fn activate(&mut self, field: AField) -> ScreenAction {
        let (_, enabled, _) = row_state(field, &self.staged);
        if !enabled && !matches!(field, AField::Backend | AField::Device) {
            return ScreenAction::Consumed; // disabled row: inert
        }
        match field {
            AField::Backend => self.open_backend_picker(),
            AField::Device => self.open_device_picker(false),
            AField::AlsaPlugin => self.open_alsa_plugin_picker(),
            AField::Dsd => self.open_dsd_picker(),
            AField::HwVolume => {
                if self.staged.alsa_hardware_volume {
                    self.staged.alsa_hardware_volume = false;
                    self.hardware_volume_status = None;
                } else {
                    self.begin_hardware_volume_probe(false);
                    return ScreenAction::ProbeHardwareVolume;
                }
            }
            AField::HwVolumeControl => {
                self.begin_hardware_volume_probe(true);
                return ScreenAction::ProbeHardwareVolume;
            }
            AField::Reserve => self.staged.reserve_dac ^= true,
            AField::Exclusive => self.staged.exclusive_mode ^= true,
            AField::Passthrough => {
                self.staged.dac_passthrough ^= true;
                cascade_on_toggle(&mut self.staged, AField::Passthrough);
            }
            AField::ForceBp => self.staged.pw_force_bitperfect ^= true,
            AField::LockOutput => self.staged.skip_sink_switch ^= true,
            AField::StreamUncached => self.staged.stream_first_track ^= true,
            AField::StreamingOnly => {
                self.staged.streaming_only ^= true;
                cascade_on_toggle(&mut self.staged, AField::StreamingOnly);
            }
            AField::Buffer => {}
        }
        ScreenAction::Consumed
    }

    fn open_backend_picker(&mut self) {
        let backends = BackendManager::available_backends();
        let options: Vec<String> = backends.iter().map(|b| backend_label(*b)).collect();
        let sel = backends
            .iter()
            .position(|b| *b == self.staged.backend)
            .unwrap_or(0);
        self.editor = Some(Editor::Backend(SelectPopup::new(
            s::A_BACKEND,
            options,
            sel,
            false,
        )));
    }

    fn open_device_picker(&mut self, filter: bool) {
        let options: Vec<String> = self
            .devices
            .iter()
            .map(|d| {
                if d.bp {
                    format!("{} {}", d.label, s::BP_BADGE)
                } else {
                    d.label.clone()
                }
            })
            .collect();
        let headers: Vec<Option<String>> = self.devices.iter().map(|d| d.header.clone()).collect();
        let sel = self
            .devices
            .iter()
            .position(|d| {
                Some(&d.id) == self.staged.output_device.as_ref()
                    || (d.id.is_empty() && self.staged.output_device.is_none())
            })
            .unwrap_or(0);
        let mut popup =
            SelectPopup::new(s::DEVICE_PICKER_TITLE, options, sel, true).with_headers(headers);
        if filter {
            popup.filter = String::new();
        }
        self.editor = Some(Editor::Device(popup));
    }

    fn open_alsa_plugin_picker(&mut self) {
        let opts = vec![
            s::ALSA_HW.to_string(),
            s::ALSA_PLUGHW.to_string(),
            s::ALSA_PCM.to_string(),
        ];
        let sel = match self.staged.alsa_plugin {
            AlsaPlugin::Hw => 0,
            AlsaPlugin::PlugHw => 1,
            AlsaPlugin::Pcm => 2,
        };
        self.editor = Some(Editor::AlsaPlugin(SelectPopup::new(
            s::A_ALSA_PLUGIN,
            opts,
            sel,
            false,
        )));
    }

    fn open_hardware_volume_picker(&mut self) {
        if self.hardware_volume_candidates.is_empty() {
            return;
        }
        let options = self
            .hardware_volume_candidates
            .iter()
            .map(hardware_volume_candidate_label)
            .collect::<Vec<_>>();
        let selected = self
            .staged
            .alsa_hardware_volume_control
            .as_ref()
            .and_then(|control| {
                self.hardware_volume_candidates
                    .iter()
                    .position(|candidate| candidate.id == *control)
            })
            .unwrap_or(0);
        self.editor = Some(Editor::HardwareVolume(SelectPopup::new(
            s::A_HW_VOLUME_CONTROL,
            options,
            selected,
            self.hardware_volume_candidates.len() > 8,
        )));
    }

    fn open_dsd_picker(&mut self) {
        let opts = vec![
            s::DSD_CONVERT.to_string(),
            s::DSD_DOP.to_string(),
            s::DSD_NATIVE.to_string(),
        ];
        let sel = match self.staged.dsd_mode.as_str() {
            "dop" => 1,
            "native" => 2,
            _ => 0,
        };
        self.editor = Some(Editor::Dsd(SelectPopup::new(s::A_DSD, opts, sel, false)));
    }

    fn handle_editor_key(&mut self, key: KeyEvent) -> ScreenAction {
        // DSD confirm modal is not a popup. Clone the values out first so the
        // borrow of self.editor ends before we mutate it.
        if matches!(self.editor, Some(Editor::DsdConfirm { .. })) {
            let (new, prev) = match &self.editor {
                Some(Editor::DsdConfirm { new, prev }) => (new.clone(), prev.clone()),
                _ => unreachable!(),
            };
            match key.code {
                KeyCode::Enter => {
                    self.staged.dsd_mode = new; // keep — user confirmed
                    self.editor = None;
                }
                KeyCode::Esc => {
                    self.staged.dsd_mode = prev; // revert (§3.2.4)
                    self.editor = None;
                }
                _ => {}
            }
            return ScreenAction::Consumed;
        }

        let editor = self.editor.take().unwrap();
        match editor {
            Editor::Backend(mut p) => match p.handle_key(key) {
                SelectOutcome::Chosen(i) => {
                    let backends = BackendManager::available_backends();
                    if let Some(nb) = backends.get(i).copied() {
                        if nb != self.staged.backend {
                            self.staged.backend = nb;
                            cascade_on_backend_change(&mut self.staged);
                            self.scanning = true;
                            return ScreenAction::RefreshDevices; // item 7 re-enum
                        }
                    }
                    ScreenAction::Consumed
                }
                SelectOutcome::Cancelled => ScreenAction::Consumed,
                SelectOutcome::Pending => {
                    self.editor = Some(Editor::Backend(p));
                    ScreenAction::Consumed
                }
            },
            Editor::Device(mut p) => match p.handle_key(key) {
                SelectOutcome::Chosen(i) => {
                    if let Some(d) = self.devices.get(i) {
                        let output_device = if d.id.is_empty() {
                            None
                        } else {
                            Some(d.id.clone())
                        };
                        if output_device != self.staged.output_device {
                            self.staged.output_device = output_device;
                            self.staged.alsa_hardware_volume = false;
                            self.staged.alsa_hardware_volume_control = None;
                            self.hardware_volume_candidates.clear();
                            self.hardware_volume_route_key = None;
                            self.hardware_volume_status = None;
                        }
                    }
                    ScreenAction::Consumed
                }
                SelectOutcome::Cancelled => ScreenAction::Consumed,
                SelectOutcome::Pending => {
                    self.editor = Some(Editor::Device(p));
                    ScreenAction::Consumed
                }
            },
            Editor::AlsaPlugin(mut p) => match p.handle_key(key) {
                SelectOutcome::Chosen(i) => {
                    let plugin = match i {
                        1 => AlsaPlugin::PlugHw,
                        2 => AlsaPlugin::Pcm,
                        _ => AlsaPlugin::Hw,
                    };
                    if plugin != self.staged.alsa_plugin {
                        self.staged.alsa_plugin = plugin;
                        self.staged.alsa_hardware_volume = false;
                        self.staged.alsa_hardware_volume_control = None;
                        self.hardware_volume_candidates.clear();
                        self.hardware_volume_route_key = None;
                        self.hardware_volume_status = None;
                    }
                    ScreenAction::Consumed
                }
                SelectOutcome::Cancelled => ScreenAction::Consumed,
                SelectOutcome::Pending => {
                    self.editor = Some(Editor::AlsaPlugin(p));
                    ScreenAction::Consumed
                }
            },
            Editor::HardwareVolume(mut p) => match p.handle_key(key) {
                SelectOutcome::Chosen(i) => {
                    if let (Some(candidate), Some(route_key)) = (
                        self.hardware_volume_candidates.get(i),
                        self.hardware_volume_route_key.clone(),
                    ) {
                        self.staged.alsa_hardware_volume_control = Some(candidate.id.clone());
                        self.staged
                            .alsa_hardware_volume_controls
                            .insert(route_key, candidate.id.clone());
                        self.staged.alsa_hardware_volume = true;
                        self.hardware_volume_status = Some(format!("selected {}", candidate.id));
                    }
                    ScreenAction::Consumed
                }
                SelectOutcome::Cancelled => ScreenAction::Consumed,
                SelectOutcome::Pending => {
                    self.editor = Some(Editor::HardwareVolume(p));
                    ScreenAction::Consumed
                }
            },
            Editor::Dsd(mut p) => match p.handle_key(key) {
                SelectOutcome::Chosen(i) => {
                    let new = match i {
                        1 => "dop",
                        2 => "native",
                        _ => "convert",
                    }
                    .to_string();
                    if new == "convert" || new == self.staged.dsd_mode {
                        self.staged.dsd_mode = new; // safe on every DAC — no confirm
                    } else {
                        // §3.2.4 guard for dop/native.
                        self.editor = Some(Editor::DsdConfirm {
                            new,
                            prev: self.staged.dsd_mode.clone(),
                        });
                    }
                    ScreenAction::Consumed
                }
                SelectOutcome::Cancelled => ScreenAction::Consumed,
                SelectOutcome::Pending => {
                    self.editor = Some(Editor::Dsd(p));
                    ScreenAction::Consumed
                }
            },
            Editor::DsdConfirm { .. } => unreachable!("handled above"),
        }
    }

    // -------------------------- render --------------------------

    pub fn draw(&self, f: &mut Frame, area: Rect, _ctx: &DrawCtx) {
        let width = area.width.saturating_sub(2); // section inner width
        let fields = visible_fields(&self.staged);
        let focused_field = fields.get(self.focus).copied();
        let active = |members: &[AField]| {
            focused_field
                .map(|ff| members.contains(&ff))
                .unwrap_or(false)
        };
        // ONE control column for the whole screen (owner's "misma área de columna").
        let labels: Vec<&str> = fields.iter().map(|f| self.field_display(*f).0).collect();
        let ctrl_col = widgets::control_column(&labels, width);

        use AField::*;
        let mut secs: Vec<widgets::Section> = Vec::new();
        let mut anchor: Option<widgets::FocusAnchor> = None;

        let out_members: &[AField] = &[Backend, Device, AlsaPlugin, HwVolume, HwVolumeControl, Dsd];
        let (mut out_lines, out_a) =
            self.group_block(&fields, out_members, focused_field, ctrl_col, width);
        if self.staged.backend == AudioBackendType::Jack {
            out_lines.extend(widgets::wrapped_note(s::JACK_WARNING, width, theme::warn()));
        }
        if !out_lines.is_empty() {
            widgets::push_section(
                &mut secs,
                &mut anchor,
                s::AUDIO_GROUP_OUTPUT,
                active(out_members),
                out_lines,
                out_a,
            );
        }

        let bp_members: &[AField] = &[Exclusive, Reserve, Passthrough, ForceBp, LockOutput];
        let (bp_lines, bp_a) =
            self.group_block(&fields, bp_members, focused_field, ctrl_col, width);
        if !bp_lines.is_empty() {
            widgets::push_section(
                &mut secs,
                &mut anchor,
                s::AUDIO_GROUP_BITPERFECT,
                active(bp_members),
                bp_lines,
                bp_a,
            );
        }

        let tr_members: &[AField] = &[StreamUncached, Buffer, StreamingOnly];
        let (tr_lines, tr_a) =
            self.group_block(&fields, tr_members, focused_field, ctrl_col, width);
        if !tr_lines.is_empty() {
            widgets::push_section(
                &mut secs,
                &mut anchor,
                s::AUDIO_GROUP_TRANSPORT,
                active(tr_members),
                tr_lines,
                tr_a,
            );
        }

        widgets::sections_scroll(f, area, &secs, anchor);

        // Overlays.
        match &self.editor {
            Some(Editor::Backend(p))
            | Some(Editor::AlsaPlugin(p))
            | Some(Editor::HardwareVolume(p))
            | Some(Editor::Dsd(p)) => p.draw(f, area),
            Some(Editor::Device(p)) => {
                if self.scanning {
                    widgets::busy_overlay(f, area, s::AUDIO_SCANNING, 0);
                } else if self.devices.len() <= 1 {
                    // Only the synthetic "System default" — the §5.1 hint panel.
                    widgets::modal(
                        f,
                        area,
                        s::DEVICE_PICKER_TITLE,
                        s::NO_DEVICES,
                        s::HELP_SELECT,
                    );
                } else {
                    p.draw(f, area);
                }
            }
            Some(Editor::DsdConfirm { .. }) => {
                widgets::modal(
                    f,
                    area,
                    s::DSD_GUARD_TITLE,
                    s::DSD_GUARD_BODY,
                    s::DSD_GUARD_HINT,
                );
            }
            None => {}
        }
    }

    /// The field blocks of one group, in declared order (skipping hidden fields).
    /// Returns the flattened lines plus, when the focused field is in this group,
    /// its (first-line, height) inside the group for follow-focus scrolling.
    fn group_block(
        &self,
        fields: &[AField],
        members: &[AField],
        focused_field: Option<AField>,
        ctrl_col: u16,
        width: u16,
    ) -> (Vec<Line<'static>>, Option<(u16, u16)>) {
        let mut lines = Vec::new();
        let mut within = None;
        for gf in members {
            if let Some(pos) = fields.iter().position(|x| x == gf) {
                let start = lines.len() as u16;
                let block = self.field_block(*gf, pos, ctrl_col, width);
                if focused_field == Some(*gf) {
                    within = Some((start, block.len() as u16));
                }
                lines.extend(block);
            }
        }
        (lines, within)
    }

    fn field_block(
        &self,
        field: AField,
        focus_pos: usize,
        ctrl_col: u16,
        width: u16,
    ) -> Vec<Line<'static>> {
        let (_, enabled, reason) = row_state(field, &self.staged);
        let focused = focus_pos == self.focus && self.editor.is_none();
        let (label, value, widget) = self.field_display(field);
        let f = widgets::Field {
            label,
            value,
            widget,
            focused,
            enabled,
            reason,
            description: None,
        };
        widgets::field_block(&f, ctrl_col, width)
    }

    fn field_display(&self, field: AField) -> (&'static str, String, &'static str) {
        let a = &self.staged;
        let on_off = |b: bool| {
            if b {
                "on".to_string()
            } else {
                "off".to_string()
            }
        };
        match field {
            AField::Backend => (s::A_BACKEND, backend_label(a.backend), "[select]"),
            AField::Device => {
                let dev = if self.scanning {
                    s::AUDIO_SCANNING.to_string()
                } else {
                    self.device_label()
                };
                (s::A_DEVICE, dev, "[select]")
            }
            AField::AlsaPlugin => (
                s::A_ALSA_PLUGIN,
                alsa_plugin_label(a.alsa_plugin).to_string(),
                "[select]",
            ),
            AField::HwVolume => {
                let value = if self.hardware_volume_probing {
                    "checking…".to_string()
                } else if let Some(status) = &self.hardware_volume_status {
                    format!("{} · {status}", on_off(a.alsa_hardware_volume))
                } else {
                    on_off(a.alsa_hardware_volume)
                };
                (s::A_HW_VOLUME, value, "[toggle]")
            }
            AField::HwVolumeControl => (
                s::A_HW_VOLUME_CONTROL,
                a.alsa_hardware_volume_control
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| "none".to_string()),
                "[select]",
            ),
            AField::Dsd => (s::A_DSD, dsd_label(&a.dsd_mode).to_string(), "[select]"),
            AField::Exclusive => (s::A_EXCLUSIVE, on_off(a.exclusive_mode), "[toggle]"),
            AField::Reserve => (s::A_RESERVE, on_off(a.reserve_dac), "[toggle]"),
            AField::Passthrough => (s::A_PASSTHROUGH, on_off(a.dac_passthrough), "[toggle]"),
            AField::ForceBp => (s::A_FORCE_BP, on_off(a.pw_force_bitperfect), "[toggle]"),
            AField::LockOutput => (s::A_LOCK_OUTPUT, on_off(a.skip_sink_switch), "[toggle]"),
            AField::StreamUncached => (
                s::A_STREAM_UNCACHED,
                on_off(a.stream_first_track),
                "[toggle]",
            ),
            AField::Buffer => (
                s::A_BUFFER,
                format!("{} s", a.stream_buffer_seconds),
                "[slider]",
            ),
            AField::StreamingOnly => (s::A_STREAMING_ONLY, on_off(a.streaming_only), "[toggle]"),
        }
    }

    fn device_label(&self) -> String {
        match &self.staged.output_device {
            None => "System default".to_string(),
            Some(id) => self
                .devices
                .iter()
                .find(|d| &d.id == id)
                .map(|d| {
                    if d.bp {
                        format!("{} {}", d.label, s::BP_BADGE)
                    } else {
                        d.label.clone()
                    }
                })
                .unwrap_or_else(|| short_device(id)),
        }
    }
}

// ============================ value/label mappers ============================

pub fn backend_label(b: AudioBackendType) -> String {
    match b {
        AudioBackendType::PipeWire => "PipeWire".to_string(),
        AudioBackendType::Alsa => "ALSA".to_string(),
        AudioBackendType::Pulse => "PulseAudio".to_string(),
        AudioBackendType::SystemDefault => "System default".to_string(),
        AudioBackendType::Jack => "JACK".to_string(),
        AudioBackendType::WasapiExclusive => "WASAPI Exclusive".to_string(),
    }
}

/// The `settings set audio.backend` value token (matches write_one's parse_backend).
fn backend_value(b: AudioBackendType) -> &'static str {
    match b {
        AudioBackendType::SystemDefault => "system",
        AudioBackendType::PipeWire => "pipewire",
        AudioBackendType::Alsa => "alsa",
        AudioBackendType::Pulse => "pulse",
        AudioBackendType::Jack => "jack",
        AudioBackendType::WasapiExclusive => "wasapi",
    }
}

fn alsa_plugin_label(p: AlsaPlugin) -> &'static str {
    match p {
        AlsaPlugin::Hw => s::ALSA_HW,
        AlsaPlugin::PlugHw => s::ALSA_PLUGHW,
        AlsaPlugin::Pcm => s::ALSA_PCM,
    }
}
fn alsa_plugin_value(p: AlsaPlugin) -> &'static str {
    match p {
        AlsaPlugin::Hw => "hw",
        AlsaPlugin::PlugHw => "plughw",
        AlsaPlugin::Pcm => "pcm",
    }
}

fn dsd_label(mode: &str) -> &'static str {
    match mode {
        "dop" => s::DSD_DOP,
        "native" => s::DSD_NATIVE,
        _ => s::DSD_CONVERT,
    }
}

fn hardware_volume_candidate_label(candidate: &HardwareVolumeCandidate) -> String {
    let range = candidate
        .db_range
        .map(|range| {
            format!(
                "{:.2}..{:.2} dB",
                range.min as f64 / 100.0,
                range.max as f64 / 100.0
            )
        })
        .or_else(|| {
            candidate
                .raw_range
                .map(|range| format!("raw {}..{}", range.min, range.max))
        })
        .unwrap_or_else(|| "range unavailable".to_string());
    let recommended = if candidate.recommended { " ★" } else { "" };
    format!(
        "{}{} · {} · {}",
        candidate.id,
        recommended,
        range,
        candidate.channels.join(", ")
    )
}

/// Compact a long device id for menu/summary lines (char-safe).
fn short_device(id: &str) -> String {
    let count = id.chars().count();
    if count <= 24 {
        id.to_string()
    } else {
        let tail: String = id.chars().skip(count - 23).collect();
        format!("…{tail}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(id: &str, name: &str, is_default: bool, is_hardware: bool) -> AudioDevice {
        AudioDevice {
            id: id.to_string(),
            name: name.to_string(),
            description: None,
            is_default,
            max_sample_rate: None,
            supported_sample_rates: None,
            device_bus: None,
            is_hardware,
        }
    }

    fn base() -> StagedAudio {
        StagedAudio::from_settings(&AudioSettings::default())
    }

    fn route_key() -> StableAlsaRouteKey {
        serde_json::from_str(r#""alsa-route-v1|card=card:D50|pcm=hw|DEV=0""#)
            .expect("valid opaque route key")
    }

    fn mixer_candidate(name: &str, index: u32) -> HardwareVolumeCandidate {
        HardwareVolumeCandidate {
            id: AlsaMixerControlId {
                name: name.to_string(),
                index,
            },
            label: format!("{name},{index}"),
            channels: vec!["front_left".to_string(), "front_right".to_string()],
            raw_range: Some(qbz_audio::alsa_hardware_volume::HardwareVolumeRange {
                min: 0,
                max: 100,
            }),
            db_range: None,
            current_values: Vec::new(),
            current_volume: Some(0.5),
            has_playback_switch: false,
            recommended: name == "Master",
            writability: qbz_audio::alsa_hardware_volume::HardwareVolumeWritability::Verified,
            rejection_reason: None,
        }
    }

    fn mixer_probe(candidates: Vec<HardwareVolumeCandidate>) -> HardwareVolumeProbe {
        HardwareVolumeProbe {
            device_id: "hw:CARD=D50,DEV=0".to_string(),
            ctl_name: "hw:CARD=D50".to_string(),
            route_key: Some(route_key()),
            candidates,
            ucm_controls: Vec::new(),
            error: None,
        }
    }

    fn alsa_screen() -> AudioState {
        let mut settings = AudioSettings::default();
        settings.backend_type = Some(AudioBackendType::Alsa);
        settings.output_device = Some("hw:CARD=D50,DEV=0".to_string());
        settings.alsa_plugin = Some(AlsaPlugin::Hw);
        AudioState::new(&settings)
    }

    // ---- cascades §3.2.3 items 1-3 (toggle) ----

    #[test]
    fn passthrough_on_forces_lock_output_off() {
        let mut a = base();
        a.skip_sink_switch = true;
        a.dac_passthrough = true;
        cascade_on_toggle(&mut a, AField::Passthrough);
        assert!(
            !a.skip_sink_switch,
            "item 1: passthrough ON forces lock-output off"
        );
    }

    #[test]
    fn passthrough_off_forces_force_bp_off() {
        let mut a = base();
        a.pw_force_bitperfect = true;
        a.dac_passthrough = false;
        cascade_on_toggle(&mut a, AField::Passthrough);
        assert!(
            !a.pw_force_bitperfect,
            "item 2: passthrough OFF forces force-BP off"
        );
    }

    #[test]
    fn streaming_only_preserves_gapless() {
        let mut a = base();
        a.gapless_enabled = true;
        a.streaming_only = true;
        cascade_on_toggle(&mut a, AField::StreamingOnly);
        assert!(a.gapless_enabled);
        a.gapless_enabled = false;
        cascade_on_toggle(&mut a, AField::StreamingOnly);
        assert!(!a.gapless_enabled);
    }

    // ---- cascades §3.2.3 items 4-7 (backend switch) ----

    #[test]
    fn backend_non_pipewire_forces_passthrough_and_force_bp_off() {
        let mut a = base();
        a.backend = AudioBackendType::PipeWire;
        a.dac_passthrough = true;
        a.pw_force_bitperfect = true;
        a.backend = AudioBackendType::Alsa;
        cascade_on_backend_change(&mut a);
        assert!(!a.dac_passthrough, "item 4");
        assert!(!a.pw_force_bitperfect, "item 4");
    }

    #[test]
    fn backend_non_alsa_forces_exclusive_off() {
        let mut a = base();
        a.exclusive_mode = true;
        a.backend = AudioBackendType::PipeWire;
        cascade_on_backend_change(&mut a);
        assert!(!a.exclusive_mode, "item 5: exclusive is ALSA-only");
    }

    #[test]
    fn backend_alsa_forces_gapless_off() {
        let mut a = base();
        a.gapless_enabled = true;
        a.backend = AudioBackendType::Alsa;
        cascade_on_backend_change(&mut a);
        assert!(!a.gapless_enabled, "item 6");
    }

    #[test]
    fn any_backend_change_resets_device_to_system_default() {
        let mut a = base();
        a.output_device = Some("hw:CARD=D30,DEV=0".to_string());
        a.backend = AudioBackendType::PipeWire;
        cascade_on_backend_change(&mut a);
        assert_eq!(
            a.output_device, None,
            "item 7: stale device id must never survive"
        );
    }

    // ---- constraint matrix §3.2.3 ----

    #[test]
    fn exclusive_enabled_only_on_alsa() {
        let mut a = base();
        a.backend = AudioBackendType::Alsa;
        assert!(row_state(AField::Exclusive, &a).1);
        a.backend = AudioBackendType::PipeWire;
        let (shown, enabled, reason) = row_state(AField::Exclusive, &a);
        assert!(shown && !enabled, "shown-but-disabled off ALSA");
        assert_eq!(reason, Some(s::R_ALSA_ONLY));
    }

    #[test]
    fn passthrough_enabled_only_on_pipewire() {
        let mut a = base();
        a.backend = AudioBackendType::PipeWire;
        assert!(row_state(AField::Passthrough, &a).1);
        a.backend = AudioBackendType::Alsa;
        assert!(!row_state(AField::Passthrough, &a).1);
    }

    #[test]
    fn force_bp_shown_only_when_passthrough_on_and_pipewire() {
        let mut a = base();
        a.backend = AudioBackendType::PipeWire;
        a.dac_passthrough = false;
        assert!(
            !row_state(AField::ForceBp, &a).0,
            "hidden when passthrough off"
        );
        a.dac_passthrough = true;
        assert!(
            row_state(AField::ForceBp, &a).0,
            "shown when passthrough on + PW"
        );
        a.backend = AudioBackendType::Alsa;
        assert!(!row_state(AField::ForceBp, &a).0, "hidden off PW");
    }

    #[test]
    fn lock_output_shown_on_pipewire_disabled_when_passthrough_on() {
        let mut a = base();
        a.backend = AudioBackendType::PipeWire;
        a.dac_passthrough = false;
        let (shown, enabled, _) = row_state(AField::LockOutput, &a);
        assert!(shown && enabled);
        a.dac_passthrough = true;
        let (shown, enabled, reason) = row_state(AField::LockOutput, &a);
        assert!(shown && !enabled);
        assert_eq!(reason, Some(s::R_PASSTHROUGH_OFF));
    }

    #[test]
    fn alsa_plugin_and_hw_volume_gating() {
        let mut a = base();
        a.backend = AudioBackendType::Alsa;
        a.alsa_plugin = AlsaPlugin::Hw;
        assert!(row_state(AField::AlsaPlugin, &a).0);
        assert!(
            row_state(AField::HwVolume, &a).0,
            "hw volume shown on ALSA hw"
        );
        a.alsa_plugin = AlsaPlugin::PlugHw;
        assert!(
            row_state(AField::HwVolume, &a).0,
            "hardware volume remains available on the direct plughw route"
        );
        a.alsa_plugin = AlsaPlugin::Pcm;
        assert!(!row_state(AField::HwVolume, &a).0, "hidden for named PCM");
        a.backend = AudioBackendType::PipeWire;
        assert!(
            !row_state(AField::AlsaPlugin, &a).0,
            "alsa plugin hidden off ALSA"
        );
    }

    #[test]
    fn buffer_shown_only_when_stream_uncached_on() {
        let mut a = base();
        a.stream_first_track = true;
        assert!(row_state(AField::Buffer, &a).0);
        a.stream_first_track = false;
        assert!(!row_state(AField::Buffer, &a).0);
    }

    // ---- device grouping §3.2.2 ----

    #[test]
    fn non_alsa_is_flat_with_system_default_first_no_headers() {
        let devices = vec![dev("pw-node-1", "USB DAC", false, true)];
        let rows = group_devices(AudioBackendType::PipeWire, devices);
        assert_eq!(rows[0].id, "", "System default leads");
        assert!(
            rows.iter().all(|r| r.header.is_none()),
            "no headers off ALSA"
        );
        assert!(rows[1].bp, "PipeWire hardware node is BP");
    }

    #[test]
    fn alsa_groups_into_four_sections_in_order() {
        let devices = vec![
            dev("plughw:CARD=D30", "Plug D30", false, false),
            dev("hw:CARD=D30,DEV=0", "Topping D30", false, false),
            dev("sysdefault:CARD=x", "Sys x", false, false),
        ];
        let rows = group_devices(AudioBackendType::Alsa, devices);
        // First section is Defaults (the synthetic system-default row).
        assert_eq!(rows[0].header.as_deref(), Some("Defaults"));
        let headers: Vec<&str> = rows.iter().filter_map(|r| r.header.as_deref()).collect();
        assert_eq!(
            headers,
            vec![
                "Defaults",
                "Bit-perfect (Hardware / Digital)",
                "Plugin Hardware",
                "Other Outputs"
            ]
        );
    }

    #[test]
    fn alsa_hw_device_gets_bp_badge() {
        let devices = vec![dev("hw:CARD=D30,DEV=0", "Topping D30", false, false)];
        let rows = group_devices(AudioBackendType::Alsa, devices);
        let d = rows.iter().find(|r| r.id.starts_with("hw:")).unwrap();
        assert!(d.bp, "hw: ALSA device is bit-perfect");
    }

    #[test]
    fn is_default_hw_lands_in_bitperfect_section_but_gets_no_badge() {
        // §3.2.2 edge case (1:1 desktop): a device with is_default=true and an
        // hw: id → Bit-perfect SECTION (grouping passes is_default=false) but NO
        // badge (badge sees is_default=true → Defaults).
        let devices = vec![dev("hw:CARD=D30,DEV=0", "Default D30", true, false)];
        let rows = group_devices(AudioBackendType::Alsa, devices);
        let d = rows.iter().find(|r| r.id.starts_with("hw:")).unwrap();
        assert!(!d.bp, "badge predicate uses REAL is_default → no [BP]");
        // Its section header (if it's first of its section) is Bit-perfect.
        assert_eq!(
            alsa_section("hw:CARD=D30,DEV=0", false, "Default D30"),
            AlsaSection::BitPerfect,
            "grouping predicate uses is_default=false → Bit-perfect section"
        );
    }

    // ---- save diff ----

    #[test]
    fn save_keys_only_emits_changed_fields() {
        let mut st = AudioState::new(&AudioSettings::default());
        assert!(st.save_keys().is_empty(), "clean screen writes nothing");
        st.staged.exclusive_mode = true;
        let keys = st.save_keys();
        assert_eq!(
            keys,
            vec![("audio.exclusive_mode".to_string(), "true".to_string())]
        );
    }

    #[test]
    fn ambiguous_probe_opens_a_closed_choice_without_staging_a_write() {
        let mut state = alsa_screen();
        let probe = mixer_probe(vec![
            mixer_candidate("Master", 0),
            mixer_candidate("PCM", 1),
        ]);
        let decision =
            qbz_audio::alsa_hardware_volume::decide_hardware_volume(&probe, &HashMap::new());

        state.set_hardware_volume_probe(Ok((probe, decision)));

        assert!(!state.staged.alsa_hardware_volume);
        assert!(state.staged.alsa_hardware_volume_control.is_none());
        assert!(matches!(state.editor, Some(Editor::HardwareVolume(_))));
        assert!(state.save_keys().is_empty(), "probing alone must not write");
    }

    #[test]
    fn unique_probe_stages_exact_identity_before_enabling() {
        let mut state = alsa_screen();
        let expected = AlsaMixerControlId {
            name: "D50 III".to_string(),
            index: 7,
        };
        let probe = mixer_probe(vec![mixer_candidate(&expected.name, expected.index)]);
        let decision =
            qbz_audio::alsa_hardware_volume::decide_hardware_volume(&probe, &HashMap::new());

        state.set_hardware_volume_probe(Ok((probe, decision)));

        assert!(state.staged.alsa_hardware_volume);
        assert_eq!(
            state.staged.alsa_hardware_volume_control.as_ref(),
            Some(&expected)
        );
        assert_eq!(
            state.staged.alsa_hardware_volume_controls.get(&route_key()),
            Some(&expected)
        );
        let keys = state.save_keys();
        assert_eq!(keys[0].0, "audio.alsa_hardware_volume_control");
        assert_eq!(keys[0].1, "D50 III,7");
        assert_eq!(keys[1].0, "audio.alsa_hardware_volume");
        assert_eq!(keys[1].1, "true");
    }

    #[test]
    fn macos_system_default_keeps_exclusive_across_a_backend_change() {
        // #748 parity with the Qt shell: System default on macOS honours
        // exclusive_mode through CoreAudio Hog Mode, so the ALSA-only cascade
        // must not clear it there. Off macOS the flag is inert and still resets.
        let mut a = base();
        a.exclusive_mode = true;
        a.backend = AudioBackendType::SystemDefault;
        cascade_on_backend_change_for(&mut a, true);
        assert!(a.exclusive_mode, "macOS keeps exclusive on System default");
        cascade_on_backend_change_for(&mut a, false);
        assert!(!a.exclusive_mode, "off macOS the cascade still clears it");
    }
}
