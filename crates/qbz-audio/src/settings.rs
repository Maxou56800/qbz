//! Audio settings persistence
//!
//! Stores user preferences for audio output device, exclusive mode, and DAC passthrough.
//!
//! NOTE: Tauri command wrappers remain in qbz-nix. This module contains only
//! the core types and persistence logic.

use crate::alsa_hardware_volume::{stable_alsa_route_key, AlsaMixerControlId, StableAlsaRouteKey};
use crate::{AlsaPlugin, AudioBackendType};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioSettings {
    pub output_device: Option<String>, // None = system default
    pub exclusive_mode: bool,
    pub dac_passthrough: bool,
    pub preferred_sample_rate: Option<u32>,     // None = auto
    pub backend_type: Option<AudioBackendType>, // None = auto-detect
    pub alsa_plugin: Option<AlsaPlugin>,        // Only used when backend is ALSA
    pub alsa_hardware_volume: bool,             // Use ALSA mixer for volume (only with hw: devices)
    /// Exact mixer identity per physical card + PCM route. The empty map is
    /// the backward-compatible value for legacy settings databases.
    #[serde(default)]
    pub alsa_hardware_volume_controls: HashMap<StableAlsaRouteKey, AlsaMixerControlId>,
    /// When true, uncached tracks start playing via streaming instead of waiting for full download
    pub stream_first_track: bool,
    /// Initial buffer size in seconds before starting streaming playback (1-10, default 3)
    pub stream_buffer_seconds: u8,
    /// When true, skip L1+L2 cache writes (streaming-only mode). Offline cache still works.
    pub streaming_only: bool,
    #[serde(default)]
    pub playback_cache: qbz_models::playback_cache::PlaybackCacheSettings,
    /// When true, cap the REQUESTED streaming quality tier at the local output
    /// device's detected ceiling (#638 fix 3; consumed by the desktop's
    /// request-time resolution, never by the audio backends). Applies to local
    /// playback only — never to casting, where the local DAC is not in the
    /// signal path. Default: false (opt-in).
    pub limit_quality_to_device: bool,
    /// Cached max sample rate of the selected device (set when device is selected)
    /// Used when limit_quality_to_device is true
    pub device_max_sample_rate: Option<u32>,
    /// Per-device sample rate limits: device_id -> max_sample_rate
    /// Allows different DACs to have independent max sample rate configurations
    #[serde(default)]
    pub device_sample_rate_limits: HashMap<String, u32>,
    /// When true, apply volume normalization using ReplayGain metadata.
    /// When false (default), the audio pipeline is 100% bit-perfect — no sample modification.
    pub normalization_enabled: bool,
    /// Target loudness in LUFS for normalization.
    /// Common values: -14.0 (Spotify/YouTube), -18.0 (audiophile), -23.0 (EBU broadcast)
    pub normalization_target_lufs: f32,
    /// When true (default), the normalization boost is capped so the track's
    /// known peak stays 1 dB under full scale; off lets quiet tracks reach
    /// the target even if their peaks clip.
    pub normalization_prevent_clipping: bool,
    /// When true, consecutive same-format tracks play without gap.
    /// Works on Rodio (PipeWire/Pulse) and ALSA Direct backends. Requires cached tracks.
    pub gapless_enabled: bool,
    /// When true, force PipeWire clock.force-quantum alongside clock.force-rate for bit-perfect.
    /// Reset both to 0 on stop. PipeWire-only, requires dac_passthrough.
    pub pw_force_bitperfect: bool,
    /// When true, reload audio settings from DB into the player on app startup.
    /// Useful when Player::new() may hold stale settings (e.g., after Flatpak updates).
    /// Default: false (most users don't need this).
    pub sync_audio_on_startup: bool,
    /// User preference for what happens when all quality retries fail.
    /// Values: "ask" (default), "always_fallback", "always_skip"
    /// Protected by ADR-003: must survive reset_all() and migrations.
    pub quality_fallback_behavior: String,
    /// When true, skip `pactl set-default-sink` on stream creation.
    /// Preserves external routing (JACK, qjackctl, Reaper).
    /// Mutually exclusive with dac_passthrough.
    pub skip_sink_switch: bool,
    /// When true, automatically try lower quality tiers if the requested one fails.
    /// When false (default), playback or download fails if the exact quality is unavailable.
    pub allow_quality_fallback: bool,
    /// When true, hold a per-process ALSA device reservation (Lifetime B) for the
    /// configured output device while QBZ is running, so other PulseAudio/PipeWire
    /// clients won't grab the DAC and break exclusive playback. Off by default.
    /// See `qbz-nix-docs/specs/2026-05-07-alsa-exclusive-hardening-design.md`.
    #[serde(default)]
    pub reserve_dac_while_running: bool,
    /// DSD delivery mode: "convert" (default — DSD→PCM, works everywhere),
    /// "dop" (DSD over PCM, opt-in: NOT detectable, wrong DAC = loud noise),
    /// or "native" (ALSA DSD_U32 formats, needs a kernel quirk for the DAC).
    /// Only takes effect on the ALSA direct backend with stereo tracks;
    /// everything else converts.
    #[serde(default = "default_dsd_mode")]
    pub dsd_mode: String,
}

fn default_dsd_mode() -> String {
    "convert".to_string()
}

impl AudioSettings {
    /// Exact control selected for the currently configured physical PCM route.
    /// A bare `alsa_hardware_volume=true` from an older database intentionally
    /// returns `None` until the shared probe resolves and persists a choice.
    pub fn selected_alsa_hardware_volume_control(&self) -> Option<AlsaMixerControlId> {
        if !self.alsa_hardware_volume {
            return None;
        }
        let route = stable_alsa_route_key(self.output_device.as_deref()?).ok()?;
        self.alsa_hardware_volume_controls.get(&route).cloned()
    }
}

impl Default for AudioSettings {
    fn default() -> Self {
        Self {
            output_device: None,
            exclusive_mode: false,
            dac_passthrough: false,
            preferred_sample_rate: None,
            // OOTB default is "System" (Some(SystemDefault)): play through the OS
            // default output, shared with other apps like any normal player — no
            // bit-perfect, no `pactl`. See AudioBackendType::default(). "Auto"
            // (None) and explicit backends (PipeWire / ALSA) are honored as-is;
            // this only sets what a fresh install and the Reset action land on.
            //
            // History: this defaulted to Some(PipeWire) on Linux to dodge a rodio
            // DeviceSink-drop-on-resume race on the CPAL path (#375), but that
            // hard-required `pactl` and froze OOTB playback without it (#470). The
            // #375 race is covered by the cpal 0.17.3 / alsa 0.11 stream-drop
            // fixes, and "System" is the app-like default audiophiles override.
            backend_type: Some(AudioBackendType::default()),
            alsa_plugin: Some(AlsaPlugin::Hw), // Default to hw (bit-perfect)
            alsa_hardware_volume: false,       // Disabled by default (maximum compatibility)
            alsa_hardware_volume_controls: HashMap::new(),
            stream_first_track: true,          // On by default (opt-out)
            stream_buffer_seconds: 2,          // 2 seconds initial buffer
            playback_cache: Default::default(),
            streaming_only: false, // Disabled by default (cache tracks for instant replay)
            limit_quality_to_device: false, // Opt-in. Off since 1.1.9 (#45); wired to the read-only probe in #638 fix 3
            device_max_sample_rate: None,   // Set when device is selected
            device_sample_rate_limits: HashMap::new(), // Per-device limits (empty = no limit)
            normalization_enabled: false,   // Off by default — preserves bit-perfect pipeline
            normalization_target_lufs: -14.0, // Spotify/YouTube standard
            normalization_prevent_clipping: true, // Peak-aware boost cap (2026-09)
            gapless_enabled: true, // On by default — works for same-format tracks on all backends
            pw_force_bitperfect: false, // Off by default — experimental PipeWire feature
            sync_audio_on_startup: false, // Off by default — opt-in for stale-settings edge case
            quality_fallback_behavior: "ask".to_string(),
            skip_sink_switch: false, // Off by default — only for JACK/DAW routing setups
            allow_quality_fallback: false, // Off by default — fail rather than silently downgrade
            reserve_dac_while_running: false, // Off by default — opt-in DAC reservation (Lifetime B)
            dsd_mode: default_dsd_mode(),     // "convert" — safe on every DAC
        }
    }
}

pub struct AudioSettingsStore {
    conn: Connection,
}

impl AudioSettingsStore {
    fn open_at(dir: &Path, db_name: &str) -> Result<Self, String> {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("Failed to create data directory: {}", e))?;

        let db_path = dir.join(db_name);
        let conn = Connection::open(&db_path)
            .map_err(|e| format!("Failed to open audio settings database: {}", e))?;

        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL;")
            .map_err(|e| format!("Failed to enable WAL for audio settings database: {}", e))?;

        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS audio_settings (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                output_device TEXT,
                exclusive_mode INTEGER NOT NULL DEFAULT 0,
                dac_passthrough INTEGER NOT NULL DEFAULT 0,
                preferred_sample_rate INTEGER,
                backend_type TEXT,
                alsa_plugin TEXT,
                alsa_hardware_volume INTEGER NOT NULL DEFAULT 0,
                alsa_hardware_volume_controls TEXT NOT NULL DEFAULT '{}',
                stream_first_track INTEGER NOT NULL DEFAULT 1,
                stream_buffer_seconds INTEGER NOT NULL DEFAULT 2
            );",
        )
        .map_err(|e| format!("Failed to create audio settings table: {}", e))?;

        // Migration: Add new columns if they don't exist (for existing databases)
        let _ = conn.execute(
            "ALTER TABLE audio_settings ADD COLUMN backend_type TEXT",
            [],
        );
        let _ = conn.execute("ALTER TABLE audio_settings ADD COLUMN alsa_plugin TEXT", []);
        let _ = conn.execute(
            "ALTER TABLE audio_settings ADD COLUMN alsa_hardware_volume INTEGER DEFAULT 0",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE audio_settings ADD COLUMN alsa_hardware_volume_controls TEXT DEFAULT '{}'",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE audio_settings ADD COLUMN stream_first_track INTEGER DEFAULT 1",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE audio_settings ADD COLUMN stream_buffer_seconds INTEGER DEFAULT 2",
            [],
        );
        let _ = conn.execute("ALTER TABLE audio_settings ADD COLUMN playback_cache TEXT NOT NULL DEFAULT '{}'", []);
        let _ = conn.execute(
            "ALTER TABLE audio_settings ADD COLUMN streaming_only INTEGER DEFAULT 0",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE audio_settings ADD COLUMN limit_quality_to_device INTEGER DEFAULT 0",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE audio_settings ADD COLUMN device_max_sample_rate INTEGER",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE audio_settings ADD COLUMN normalization_enabled INTEGER DEFAULT 0",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE audio_settings ADD COLUMN normalization_target_lufs REAL DEFAULT -14.0",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE audio_settings ADD COLUMN normalization_prevent_clipping INTEGER DEFAULT 1",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE audio_settings ADD COLUMN gapless_enabled INTEGER DEFAULT 0",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE audio_settings ADD COLUMN device_sample_rate_limits TEXT",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE audio_settings ADD COLUMN pw_force_bitperfect INTEGER DEFAULT 0",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE audio_settings ADD COLUMN sync_audio_on_startup INTEGER DEFAULT 0",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE audio_settings ADD COLUMN quality_fallback_behavior TEXT DEFAULT 'ask'",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE audio_settings ADD COLUMN skip_sink_switch INTEGER DEFAULT 0",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE audio_settings ADD COLUMN allow_quality_fallback INTEGER DEFAULT 0",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE audio_settings ADD COLUMN reserve_dac_while_running INTEGER DEFAULT 0",
            [],
        );
        let _ = conn.execute(
            "ALTER TABLE audio_settings ADD COLUMN dsd_mode TEXT DEFAULT 'convert'",
            [],
        );

        // Seed the single settings row on first run with the OOTB default backend
        // ("System"). INSERT OR IGNORE is a one-time seed: it only fires when the
        // row does not exist yet, so existing installs are never rewritten on
        // later launches (settings are only reset by the explicit Reset action,
        // never just by restarting).
        //
        // There is deliberately NO backfill of existing NULL backend_type rows: a
        // NULL backend_type means "Auto" and is preserved as-is. (An earlier #375
        // workaround backfilled NULL -> PipeWire on every open, which hard-required
        // `pactl` and froze OOTB playback without it, #470.)
        let default_backend_json = serde_json::to_string(&AudioBackendType::default())
            .map_err(|e| format!("Failed to serialize default backend: {}", e))?;
        conn.execute(
            "INSERT OR IGNORE INTO audio_settings (id, exclusive_mode, dac_passthrough, backend_type) VALUES (1, 0, 0, ?1)",
            params![default_backend_json],
        )
        .map_err(|e| format!("Failed to seed audio settings row: {}", e))?;

        // One-time backfill (#638 Phase C / F10): installs that first ran a
        // pre-#45 build had `limit_quality_to_device` backfilled to 1 by the
        // original DEFAULT-1 migration and still read `true` today. That was
        // inert while nothing consumed the flag; now that the local device cap
        // (fix 3) consumes it, those installs would silently gain a cap nobody
        // asked for on upgrade — a silently-appearing cap is the exact bug
        // class this work removes. Reset the flag to the modern default ONCE,
        // gated on `user_version`, so a user who deliberately re-enables it
        // afterwards is never clobbered again. The stamp is only written after
        // a successful UPDATE so a failed backfill retries on the next open.
        let user_version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .unwrap_or(0);
        if user_version < 1 {
            match conn.execute(
                "UPDATE audio_settings SET limit_quality_to_device = 0 WHERE id = 1",
                [],
            ) {
                Ok(_) => {
                    if let Err(e) = conn.pragma_update(None, "user_version", 1) {
                        log::warn!("audio settings: user_version stamp failed: {e}");
                    }
                }
                Err(e) => {
                    log::warn!("audio settings: limit_quality_to_device backfill failed: {e}")
                }
            }
        }

        Ok(Self { conn })
    }

    pub fn new() -> Result<Self, String> {
        let data_dir = dirs::data_dir()
            .ok_or("Could not determine data directory")?
            .join("qbz");
        Self::open_at(&data_dir, "audio_settings.db")
    }

    pub fn new_at(base_dir: &Path) -> Result<Self, String> {
        Self::open_at(base_dir, "audio_settings.db")
    }

    pub fn get_settings(&self) -> Result<AudioSettings, String> {
        self.conn
            .query_row(
                "SELECT output_device, exclusive_mode, dac_passthrough, preferred_sample_rate, backend_type, alsa_plugin, alsa_hardware_volume, stream_first_track, stream_buffer_seconds, streaming_only, limit_quality_to_device, device_max_sample_rate, normalization_enabled, normalization_target_lufs, gapless_enabled, device_sample_rate_limits, pw_force_bitperfect, sync_audio_on_startup, quality_fallback_behavior, skip_sink_switch, allow_quality_fallback, reserve_dac_while_running, dsd_mode, alsa_hardware_volume_controls, playback_cache, normalization_prevent_clipping FROM audio_settings WHERE id = 1",
                [],
                |row| {
                    // Parse backend_type from JSON string
                    let backend_type: Option<AudioBackendType> = row
                        .get::<_, Option<String>>(4)?
                        .and_then(|s| serde_json::from_str(&s).ok());

                    // Parse alsa_plugin from JSON string
                    let alsa_plugin: Option<AlsaPlugin> = row
                        .get::<_, Option<String>>(5)?
                        .and_then(|s| serde_json::from_str(&s).ok());

                    // Parse device_sample_rate_limits from JSON string
                    let device_sample_rate_limits: HashMap<String, u32> = row
                        .get::<_, Option<String>>(15)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default();

                    let alsa_hardware_volume_controls: HashMap<
                        StableAlsaRouteKey,
                        AlsaMixerControlId,
                    > = row
                        .get::<_, Option<String>>(23)?
                        .and_then(|s| serde_json::from_str(&s).ok())
                        .unwrap_or_default();

                    Ok(AudioSettings {
                        output_device: row.get(0)?,
                        exclusive_mode: row.get::<_, i64>(1)? != 0,
                        dac_passthrough: row.get::<_, i64>(2)? != 0,
                        preferred_sample_rate: row.get(3)?,
                        backend_type,
                        alsa_plugin,
                        alsa_hardware_volume: row.get::<_, Option<i64>>(6)?.unwrap_or(0) != 0,
                        alsa_hardware_volume_controls,
                        stream_first_track: row.get::<_, Option<i64>>(7)?.unwrap_or(0) != 0,
                        stream_buffer_seconds: row.get::<_, Option<i64>>(8)?.unwrap_or(3) as u8,
                        playback_cache: row.get::<_, Option<String>>(24)?.and_then(|s| serde_json::from_str::<qbz_models::playback_cache::PlaybackCacheSettings>(&s).ok()).filter(|p| p.validate().is_ok()).unwrap_or_default(),
                        streaming_only: row.get::<_, Option<i64>>(9)?.unwrap_or(0) != 0,
                        limit_quality_to_device: row.get::<_, Option<i64>>(10)?.unwrap_or(0) != 0,
                        device_max_sample_rate: row.get::<_, Option<i64>>(11)?.map(|r| r as u32),
                        device_sample_rate_limits,
                        normalization_enabled: row.get::<_, Option<i64>>(12)?.unwrap_or(0) != 0,
                        normalization_target_lufs: row.get::<_, Option<f64>>(13)?.unwrap_or(-14.0) as f32,
                        normalization_prevent_clipping: row.get::<_, Option<i64>>(25)?.unwrap_or(1) != 0,
                        gapless_enabled: row.get::<_, Option<i64>>(14)?.unwrap_or(0) != 0,
                        pw_force_bitperfect: row.get::<_, Option<i64>>(16)?.unwrap_or(0) != 0,
                        sync_audio_on_startup: row.get::<_, Option<i64>>(17)?.unwrap_or(0) != 0,
                        quality_fallback_behavior: row
                            .get::<_, Option<String>>(18)?
                            .unwrap_or_else(|| "ask".to_string()),
                        skip_sink_switch: row.get::<_, Option<i64>>(19)?.unwrap_or(0) != 0,
                        allow_quality_fallback: row.get::<_, Option<i64>>(20)?.unwrap_or(0) != 0,
                        reserve_dac_while_running: row
                            .get::<_, Option<i64>>(21)?
                            .unwrap_or(0)
                            != 0,
                        dsd_mode: row
                            .get::<_, Option<String>>(22)?
                            .unwrap_or_else(default_dsd_mode),
                    })
                },
            )
            .map_err(|e| format!("Failed to get audio settings: {}", e))
    }

    pub fn set_output_device(&self, device: Option<&str>) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE audio_settings SET output_device = ?1 WHERE id = 1",
                params![device],
            )
            .map_err(|e| format!("Failed to set output device: {}", e))?;
        Ok(())
    }

    pub fn set_exclusive_mode(&self, enabled: bool) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE audio_settings SET exclusive_mode = ?1 WHERE id = 1",
                params![enabled as i64],
            )
            .map_err(|e| format!("Failed to set exclusive mode: {}", e))?;
        Ok(())
    }

    pub fn set_dac_passthrough(&self, enabled: bool) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE audio_settings SET dac_passthrough = ?1 WHERE id = 1",
                params![enabled as i64],
            )
            .map_err(|e| format!("Failed to set DAC passthrough: {}", e))?;
        Ok(())
    }

    pub fn set_sample_rate(&self, rate: Option<u32>) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE audio_settings SET preferred_sample_rate = ?1 WHERE id = 1",
                params![rate.map(|r| r as i64)],
            )
            .map_err(|e| format!("Failed to set sample rate: {}", e))?;
        Ok(())
    }

    pub fn set_backend_type(&self, backend: Option<AudioBackendType>) -> Result<(), String> {
        let backend_json = backend
            .map(|b| serde_json::to_string(&b))
            .transpose()
            .map_err(|e| format!("Failed to serialize backend type: {}", e))?;

        self.conn
            .execute(
                "UPDATE audio_settings SET backend_type = ?1 WHERE id = 1",
                params![backend_json],
            )
            .map_err(|e| format!("Failed to set backend type: {}", e))?;

        // When switching to ALSA, ensure alsa_plugin has a value (default to Hw)
        if backend == Some(AudioBackendType::Alsa) {
            let current_plugin: Option<String> = self
                .conn
                .query_row(
                    "SELECT alsa_plugin FROM audio_settings WHERE id = 1",
                    [],
                    |row| row.get(0),
                )
                .map_err(|e| format!("Failed to check alsa_plugin: {}", e))?;

            if current_plugin.is_none() {
                log::info!(
                    "[AudioSettings] ALSA backend selected with no plugin set, defaulting to Hw"
                );
                self.set_alsa_plugin(Some(AlsaPlugin::Hw))?;
            }
        }
        Ok(())
    }

    pub fn set_alsa_plugin(&self, plugin: Option<AlsaPlugin>) -> Result<(), String> {
        let plugin_json = plugin
            .map(|p| serde_json::to_string(&p))
            .transpose()
            .map_err(|e| format!("Failed to serialize ALSA plugin: {}", e))?;

        self.conn
            .execute(
                "UPDATE audio_settings SET alsa_plugin = ?1 WHERE id = 1",
                params![plugin_json],
            )
            .map_err(|e| format!("Failed to set ALSA plugin: {}", e))?;
        Ok(())
    }

    pub fn set_alsa_hardware_volume(&self, enabled: bool) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE audio_settings SET alsa_hardware_volume = ?1 WHERE id = 1",
                params![enabled as i64],
            )
            .map_err(|e| format!("Failed to set ALSA hardware volume: {}", e))?;
        Ok(())
    }

    /// Persist an exact control for one stable route without changing whether
    /// hardware volume is enabled. Used by the explicit "Change control" flow.
    pub fn set_alsa_hardware_volume_control(
        &self,
        route: StableAlsaRouteKey,
        control: AlsaMixerControlId,
    ) -> Result<(), String> {
        let mut controls = self.get_settings()?.alsa_hardware_volume_controls;
        controls.insert(route, control);
        let json = serde_json::to_string(&controls)
            .map_err(|error| format!("Failed to serialize ALSA mixer controls: {error}"))?;
        self.conn
            .execute(
                "UPDATE audio_settings SET alsa_hardware_volume_controls = ?1 WHERE id = 1",
                params![json],
            )
            .map_err(|error| format!("Failed to set ALSA mixer control: {error}"))?;
        Ok(())
    }

    /// Replace the route-scoped exact-control map (settings bundle import).
    /// This stores identities only; it never probes or writes a mixer. The
    /// normal startup/reload reconciliation validates the active route before
    /// hardware volume can be used.
    pub fn set_alsa_hardware_volume_controls(
        &self,
        controls: HashMap<StableAlsaRouteKey, AlsaMixerControlId>,
    ) -> Result<(), String> {
        let json = serde_json::to_string(&controls)
            .map_err(|error| format!("Failed to serialize ALSA mixer controls: {error}"))?;
        self.conn
            .execute(
                "UPDATE audio_settings SET alsa_hardware_volume_controls = ?1 WHERE id = 1",
                params![json],
            )
            .map_err(|error| format!("Failed to replace ALSA mixer controls: {error}"))?;
        Ok(())
    }

    /// Remove the exact control for one route. If that route is currently in
    /// use, callers must disable hardware volume first so no enabled setting
    /// can remain without an identity.
    pub fn clear_alsa_hardware_volume_control(
        &self,
        route: &StableAlsaRouteKey,
    ) -> Result<(), String> {
        let mut controls = self.get_settings()?.alsa_hardware_volume_controls;
        controls.remove(route);
        let json = serde_json::to_string(&controls)
            .map_err(|error| format!("Failed to serialize ALSA mixer controls: {error}"))?;
        self.conn
            .execute(
                "UPDATE audio_settings SET alsa_hardware_volume_controls = ?1 WHERE id = 1",
                params![json],
            )
            .map_err(|error| format!("Failed to clear ALSA mixer control: {error}"))?;
        Ok(())
    }

    /// Atomically persist the exact selection and enable the preference.
    pub fn enable_alsa_hardware_volume(
        &self,
        route: StableAlsaRouteKey,
        control: AlsaMixerControlId,
    ) -> Result<(), String> {
        let mut controls = self.get_settings()?.alsa_hardware_volume_controls;
        controls.insert(route, control);
        let json = serde_json::to_string(&controls)
            .map_err(|error| format!("Failed to serialize ALSA mixer controls: {error}"))?;
        self.conn
            .execute(
                "UPDATE audio_settings SET alsa_hardware_volume_controls = ?1, alsa_hardware_volume = 1 WHERE id = 1",
                params![json],
            )
            .map_err(|error| format!("Failed to enable ALSA hardware volume: {error}"))?;
        Ok(())
    }

    pub fn set_stream_first_track(&self, enabled: bool) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE audio_settings SET stream_first_track = ?1 WHERE id = 1",
                params![enabled as i64],
            )
            .map_err(|e| format!("Failed to set stream first track: {}", e))?;
        Ok(())
    }

    pub fn set_stream_buffer_seconds(&self, seconds: u8) -> Result<(), String> {
        // Clamp to valid range 1-10
        let clamped = seconds.clamp(1, 10);
        self.conn
            .execute(
                "UPDATE audio_settings SET stream_buffer_seconds = ?1 WHERE id = 1",
                params![clamped as i64],
            )
            .map_err(|e| format!("Failed to set stream buffer seconds: {}", e))?;
        Ok(())
    }

    pub fn set_playback_cache(&self, policy: &qbz_models::playback_cache::PlaybackCacheSettings) -> Result<(), String> {
        policy.validate()?;
        let json = serde_json::to_string(policy).map_err(|e| e.to_string())?;
        self.conn.execute("UPDATE audio_settings SET playback_cache = ?1 WHERE id = 1", params![json])
            .map_err(|e| format!("Failed to save playback cache settings: {e}"))?;
        Ok(())
    }

    pub fn set_streaming_only(&self, enabled: bool) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE audio_settings SET streaming_only = ?1 WHERE id = 1",
                params![enabled as i64],
            )
            .map_err(|e| format!("Failed to set streaming only: {}", e))?;
        Ok(())
    }

    pub fn set_limit_quality_to_device(&self, enabled: bool) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE audio_settings SET limit_quality_to_device = ?1 WHERE id = 1",
                params![enabled as i64],
            )
            .map_err(|e| format!("Failed to set limit quality to device: {}", e))?;
        Ok(())
    }

    pub fn set_device_max_sample_rate(&self, rate: Option<u32>) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE audio_settings SET device_max_sample_rate = ?1 WHERE id = 1",
                params![rate.map(|r| r as i64)],
            )
            .map_err(|e| format!("Failed to set device max sample rate: {}", e))?;
        Ok(())
    }

    /// Set the sample rate limit for a specific device
    /// If rate is None, removes the limit for that device
    pub fn set_device_sample_rate_limit(
        &self,
        device_id: &str,
        rate: Option<u32>,
    ) -> Result<(), String> {
        // Get current limits
        let mut limits = self.get_device_sample_rate_limits()?;

        // Update or remove the limit for this device
        if let Some(r) = rate {
            limits.insert(device_id.to_string(), r);
        } else {
            limits.remove(device_id);
        }

        // Serialize and save
        let json = serde_json::to_string(&limits)
            .map_err(|e| format!("Failed to serialize device sample rate limits: {}", e))?;

        self.conn
            .execute(
                "UPDATE audio_settings SET device_sample_rate_limits = ?1 WHERE id = 1",
                params![json],
            )
            .map_err(|e| format!("Failed to set device sample rate limits: {}", e))?;
        Ok(())
    }

    /// Get the sample rate limit for a specific device
    /// Returns None if no limit is set for this device
    pub fn get_device_sample_rate_limit(&self, device_id: &str) -> Result<Option<u32>, String> {
        let limits = self.get_device_sample_rate_limits()?;
        Ok(limits.get(device_id).copied())
    }

    /// Get all device sample rate limits
    fn get_device_sample_rate_limits(&self) -> Result<HashMap<String, u32>, String> {
        let json: Option<String> = self
            .conn
            .query_row(
                "SELECT device_sample_rate_limits FROM audio_settings WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .map_err(|e| format!("Failed to get device sample rate limits: {}", e))?;

        match json {
            Some(s) if !s.is_empty() => serde_json::from_str(&s)
                .map_err(|e| format!("Failed to parse device sample rate limits: {}", e)),
            _ => Ok(HashMap::new()),
        }
    }

    pub fn set_normalization_enabled(&self, enabled: bool) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE audio_settings SET normalization_enabled = ?1 WHERE id = 1",
                params![enabled as i64],
            )
            .map_err(|e| format!("Failed to set normalization enabled: {}", e))?;
        Ok(())
    }

    pub fn set_gapless_enabled(&self, enabled: bool) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE audio_settings SET gapless_enabled = ?1 WHERE id = 1",
                params![enabled as i64],
            )
            .map_err(|e| format!("Failed to set gapless enabled: {}", e))?;
        Ok(())
    }

    pub fn set_allow_quality_fallback(&self, enabled: bool) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE audio_settings SET allow_quality_fallback = ?1 WHERE id = 1",
                params![enabled as i64],
            )
            .map_err(|e| format!("Failed to set allow_quality_fallback: {}", e))?;
        Ok(())
    }

    pub fn set_skip_sink_switch(&self, enabled: bool) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE audio_settings SET skip_sink_switch = ?1 WHERE id = 1",
                params![enabled as i64],
            )
            .map_err(|e| format!("Failed to set skip_sink_switch: {}", e))?;
        Ok(())
    }

    /// Persist the `reserve_dac_while_running` flag (Lifetime B from the
    /// ALSA exclusive-hardening design spec). Toggling this only updates
    /// the DB row; applying the change to the live `DeviceReservation`
    /// guard is the caller's responsibility.
    pub fn set_reserve_dac_while_running(&self, enabled: bool) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE audio_settings SET reserve_dac_while_running = ?1 WHERE id = 1",
                params![enabled as i64],
            )
            .map_err(|e| format!("Failed to set reserve_dac_while_running: {}", e))?;
        Ok(())
    }

    /// Persist the DSD delivery mode ("convert" | "dop" | "native", DSD plan
    /// Phases 2-3). Deliberately NOT part of reset_all's UPDATE.
    pub fn set_dsd_mode(&self, mode: &str) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE audio_settings SET dsd_mode = ?1 WHERE id = 1",
                params![mode],
            )
            .map_err(|e| format!("Failed to set dsd_mode: {}", e))?;
        Ok(())
    }

    pub fn set_pw_force_bitperfect(&self, enabled: bool) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE audio_settings SET pw_force_bitperfect = ?1 WHERE id = 1",
                params![enabled as i64],
            )
            .map_err(|e| format!("Failed to set pw_force_bitperfect: {}", e))?;
        Ok(())
    }

    pub fn set_sync_audio_on_startup(&self, enabled: bool) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE audio_settings SET sync_audio_on_startup = ?1 WHERE id = 1",
                params![enabled as i64],
            )
            .map_err(|e| format!("Failed to set sync_audio_on_startup: {}", e))?;
        Ok(())
    }

    pub fn get_quality_fallback_behavior(&self) -> Result<String, String> {
        let settings = self.get_settings()?;
        let value = &settings.quality_fallback_behavior;
        match value.as_str() {
            "ask" | "always_fallback" | "always_skip" => Ok(value.clone()),
            _ => Ok("ask".to_string()),
        }
    }

    pub fn set_quality_fallback_behavior(&self, behavior: &str) -> Result<(), String> {
        match behavior {
            "ask" | "always_fallback" | "always_skip" => {}
            _ => return Err(format!("Invalid quality_fallback_behavior: {}", behavior)),
        }
        self.conn
            .execute(
                "UPDATE audio_settings SET quality_fallback_behavior = ?1 WHERE id = 1",
                params![behavior],
            )
            .map_err(|e| format!("Failed to set quality_fallback_behavior: {}", e))?;
        Ok(())
    }

    pub fn set_normalization_target_lufs(&self, target: f32) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE audio_settings SET normalization_target_lufs = ?1 WHERE id = 1",
                params![target as f64],
            )
            .map_err(|e| format!("Failed to set normalization target LUFS: {}", e))?;
        Ok(())
    }

    pub fn set_normalization_prevent_clipping(&self, on: bool) -> Result<(), String> {
        self.conn
            .execute(
                "UPDATE audio_settings SET normalization_prevent_clipping = ?1 WHERE id = 1",
                params![on as i64],
            )
            .map_err(|e| format!("Failed to set normalization clipping guard: {}", e))?;
        Ok(())
    }

    /// Reset all audio settings to their default values
    pub fn reset_all(&self) -> Result<AudioSettings, String> {
        // ADR-003: quality_fallback_behavior must survive reset_all()
        let saved_fallback = self
            .get_quality_fallback_behavior()
            .unwrap_or_else(|_| "ask".to_string());

        let defaults = AudioSettings::default();
        let backend_json: Option<String> = defaults
            .backend_type
            .map(|b| serde_json::to_string(&b))
            .transpose()
            .map_err(|e| format!("Failed to serialize backend type: {}", e))?;
        let plugin_json: Option<String> = defaults
            .alsa_plugin
            .map(|p| serde_json::to_string(&p))
            .transpose()
            .map_err(|e| format!("Failed to serialize ALSA plugin: {}", e))?;

        // Serialize per-device limits (empty on reset)
        let limits_json = serde_json::to_string(&defaults.device_sample_rate_limits)
            .map_err(|e| format!("Failed to serialize device sample rate limits: {}", e))?;
        let mixer_controls_json = serde_json::to_string(&defaults.alsa_hardware_volume_controls)
            .map_err(|e| format!("Failed to serialize ALSA mixer controls: {e}"))?;

        self.conn
            .execute(
                "UPDATE audio_settings SET
                    output_device = ?1,
                    exclusive_mode = ?2,
                    dac_passthrough = ?3,
                    preferred_sample_rate = ?4,
                    backend_type = ?5,
                    alsa_plugin = ?6,
                    alsa_hardware_volume = ?7,
                    stream_first_track = ?8,
                    stream_buffer_seconds = ?9,
                    streaming_only = ?10,
                    limit_quality_to_device = ?11,
                    device_max_sample_rate = ?12,
                    normalization_enabled = ?13,
                    normalization_target_lufs = ?14,
                    gapless_enabled = ?15,
                    device_sample_rate_limits = ?16,
                    pw_force_bitperfect = ?17,
                    sync_audio_on_startup = ?18,
                    skip_sink_switch = ?19,
                    allow_quality_fallback = ?20,
                    reserve_dac_while_running = ?21,
                    alsa_hardware_volume_controls = ?22,
                    normalization_prevent_clipping = ?23,
                    playback_cache = '{}'
                WHERE id = 1",
                params![
                    defaults.output_device,
                    defaults.exclusive_mode as i64,
                    defaults.dac_passthrough as i64,
                    defaults.preferred_sample_rate.map(|r| r as i64),
                    backend_json,
                    plugin_json,
                    defaults.alsa_hardware_volume as i64,
                    defaults.stream_first_track as i64,
                    defaults.stream_buffer_seconds as i64,
                    defaults.streaming_only as i64,
                    defaults.limit_quality_to_device as i64,
                    defaults.device_max_sample_rate.map(|r| r as i64),
                    defaults.normalization_enabled as i64,
                    defaults.normalization_target_lufs as f64,
                    defaults.gapless_enabled as i64,
                    limits_json,
                    defaults.pw_force_bitperfect as i64,
                    defaults.sync_audio_on_startup as i64,
                    defaults.skip_sink_switch as i64,
                    defaults.allow_quality_fallback as i64,
                    defaults.reserve_dac_while_running as i64,
                    mixer_controls_json,
                    defaults.normalization_prevent_clipping as i64,
                ],
            )
            .map_err(|e| format!("Failed to reset audio settings: {}", e))?;

        // ADR-003: restore quality_fallback_behavior after reset (it is not an audio config)
        self.conn
            .execute(
                "UPDATE audio_settings SET quality_fallback_behavior = ?1 WHERE id = 1",
                params![saved_fallback],
            )
            .map_err(|e| {
                format!(
                    "Failed to restore quality_fallback_behavior after reset: {}",
                    e
                )
            })?;

        let mut result = defaults;
        result.quality_fallback_behavior = saved_fallback;
        Ok(result)
    }
}

/// Thread-safe wrapper
pub struct AudioSettingsState {
    pub store: Arc<Mutex<Option<AudioSettingsStore>>>,
}

impl AudioSettingsState {
    pub fn new() -> Result<Self, String> {
        Ok(Self {
            store: Arc::new(Mutex::new(Some(AudioSettingsStore::new()?))),
        })
    }

    pub fn new_empty() -> Self {
        Self {
            store: Arc::new(Mutex::new(None)),
        }
    }

    pub fn init_at(&self, base_dir: &Path) -> Result<(), String> {
        let new_store = AudioSettingsStore::new_at(base_dir)?;
        let mut guard = self
            .store
            .lock()
            .map_err(|_| "Failed to lock audio settings store".to_string())?;
        *guard = Some(new_store);
        Ok(())
    }

    pub fn teardown(&self) -> Result<(), String> {
        let mut guard = self
            .store
            .lock()
            .map_err(|_| "Failed to lock audio settings store".to_string())?;
        *guard = None;
        Ok(())
    }
}

impl Default for AudioSettingsState {
    fn default() -> Self {
        Self::new_empty()
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
        std::env::temp_dir().join(format!("qbz-audio-{name}-{}-{nonce}", std::process::id()))
    }

    fn fresh_store(name: &str) -> (std::path::PathBuf, AudioSettingsStore) {
        let dir = unique_test_dir(name);
        let store = AudioSettingsStore::new_at(&dir).expect("open store in temp dir");
        (dir, store)
    }

    #[test]
    fn playback_cache_persists_validates_and_resets_without_losing_other_settings() {
        use qbz_models::playback_cache::PlaybackCacheSettings;
        let (dir, store) = fresh_store("playback-cache");
        let policy = PlaybackCacheSettings { dynamic: true, min_mib: Some(400), max_mib: Some(1600), ..Default::default() };
        store.set_playback_cache(&policy).unwrap();
        store.set_gapless_enabled(true).unwrap();
        drop(store);
        let store = AudioSettingsStore::new_at(&dir).unwrap();
        assert_eq!(store.get_settings().unwrap().playback_cache, policy);
        let invalid = PlaybackCacheSettings { max_mib: Some(100), ..policy.clone() };
        assert!(store.set_playback_cache(&invalid).is_err());
        assert_eq!(store.get_settings().unwrap().playback_cache, policy);
        store.set_quality_fallback_behavior("always_skip").unwrap();
        store.reset_all().unwrap();
        let fresh = store.get_settings().unwrap();
        assert_eq!(fresh.playback_cache, PlaybackCacheSettings::default());
        assert_eq!(fresh.quality_fallback_behavior, "always_skip");
        drop(store);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn audio_settings_default_values_are_stable() {
        let settings = AudioSettings::default();

        // OOTB default is "System" — the OS default output (#470).
        assert_eq!(settings.backend_type, Some(AudioBackendType::SystemDefault));
        assert_eq!(settings.alsa_plugin, Some(AlsaPlugin::Hw));
        assert!(settings.gapless_enabled);
        assert!(!settings.sync_audio_on_startup);
        assert_eq!(settings.quality_fallback_behavior, "ask");
        assert!(!settings.skip_sink_switch);
        assert!(!settings.allow_quality_fallback);
        assert!(!settings.reserve_dac_while_running);
    }

    #[test]
    fn audio_settings_store_returns_current_defaults() {
        let (dir, store) = fresh_store("defaults");

        let settings = store.get_settings().expect("get settings");

        // Fresh store is seeded with the OOTB default backend "System" (#470).
        assert_eq!(settings.backend_type, Some(AudioBackendType::SystemDefault));
        assert_eq!(settings.alsa_plugin, None);
        assert!(!settings.gapless_enabled);
        assert_eq!(settings.quality_fallback_behavior, "ask");
        assert!(!settings.reserve_dac_while_running);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn backend_null_stays_auto_on_reopen() {
        // A NULL backend_type means "Auto" (system default output). It must be
        // preserved across restarts — never backfilled to a concrete backend on
        // store open (the old #375 backfill hard-coded PipeWire and froze OOTB
        // playback on hosts without `pactl`, #470). Only the explicit Reset
        // action rewrites settings.
        let dir = unique_test_dir("backend-null-auto");
        {
            let store = AudioSettingsStore::new_at(&dir).expect("open store");
            store
                .conn
                .execute(
                    "UPDATE audio_settings SET backend_type = NULL WHERE id = 1",
                    [],
                )
                .expect("force null (Auto) backend");
        }

        let reopened = AudioSettingsStore::new_at(&dir).expect("reopen store");
        let settings = reopened.get_settings().expect("get settings");

        assert_eq!(settings.backend_type, None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn switching_to_alsa_sets_default_plugin_when_missing() {
        let (dir, store) = fresh_store("alsa-plugin-default");
        store.set_alsa_plugin(None).expect("clear alsa plugin");

        store
            .set_backend_type(Some(AudioBackendType::Alsa))
            .expect("set backend");
        let settings = store.get_settings().expect("get settings");

        assert_eq!(settings.backend_type, Some(AudioBackendType::Alsa));
        assert_eq!(settings.alsa_plugin, Some(AlsaPlugin::Hw));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn stream_buffer_seconds_clamps_to_valid_range() {
        let (dir, store) = fresh_store("stream-buffer-clamp");

        store.set_stream_buffer_seconds(0).expect("set low buffer");
        assert_eq!(
            store
                .get_settings()
                .expect("get settings")
                .stream_buffer_seconds,
            1
        );

        store
            .set_stream_buffer_seconds(99)
            .expect("set high buffer");
        assert_eq!(
            store
                .get_settings()
                .expect("get settings")
                .stream_buffer_seconds,
            10
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn quality_fallback_invalid_value_reads_as_ask() {
        let (dir, store) = fresh_store("quality-invalid");
        store
            .conn
            .execute(
                "UPDATE audio_settings SET quality_fallback_behavior = 'bad-value' WHERE id = 1",
                [],
            )
            .expect("write invalid fallback behavior");

        assert_eq!(
            store
                .get_quality_fallback_behavior()
                .expect("get quality fallback"),
            "ask"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn reset_all_preserves_quality_fallback_behavior() {
        let (dir, store) = fresh_store("reset-preserves-quality");
        store
            .set_quality_fallback_behavior("always_skip")
            .expect("set quality fallback");
        store.set_output_device(Some("hw:4,0")).expect("set device");
        store.set_dac_passthrough(true).expect("set dac");

        let reset = store.reset_all().expect("reset settings");
        let settings = store.get_settings().expect("get settings");

        assert_eq!(reset.quality_fallback_behavior, "always_skip");
        assert_eq!(settings.quality_fallback_behavior, "always_skip");
        assert_eq!(settings.output_device, None);
        assert!(!settings.dac_passthrough);
        assert!(settings.alsa_hardware_volume_controls.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn settings_persist_and_reopen_all_new_fields() {
        let dir = unique_test_dir("persist-new-fields");
        {
            let store = AudioSettingsStore::new_at(&dir).expect("open store");
            store
                .set_output_device(Some("hw:CARD=TestDAC,DEV=0"))
                .expect("set output device");
            store
                .set_sync_audio_on_startup(true)
                .expect("set sync flag");
            store
                .set_quality_fallback_behavior("always_fallback")
                .expect("set quality fallback");
            store
                .set_reserve_dac_while_running(true)
                .expect("set reserve flag");
            store
                .set_allow_quality_fallback(true)
                .expect("set allow fallback");
            store
                .set_skip_sink_switch(true)
                .expect("set skip sink switch");
        }

        let reopened = AudioSettingsStore::new_at(&dir).expect("reopen store");
        let settings = reopened.get_settings().expect("get settings");

        assert_eq!(
            settings.output_device.as_deref(),
            Some("hw:CARD=TestDAC,DEV=0")
        );
        assert!(settings.sync_audio_on_startup);
        assert_eq!(settings.quality_fallback_behavior, "always_fallback");
        assert!(settings.reserve_dac_while_running);
        assert!(settings.allow_quality_fallback);
        assert!(settings.skip_sink_switch);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn deserializes_legacy_json_without_reserve_dac_field() {
        let legacy = r#"{
            "output_device": null,
            "exclusive_mode": false,
            "dac_passthrough": false,
            "preferred_sample_rate": null,
            "backend_type": null,
            "alsa_plugin": null,
            "alsa_hardware_volume": false,
            "stream_first_track": false,
            "stream_buffer_seconds": 3,
            "streaming_only": false,
            "limit_quality_to_device": false,
            "device_max_sample_rate": null,
            "normalization_enabled": false,
            "normalization_target_lufs": -14.0,
            "normalization_prevent_clipping": true,
            "gapless_enabled": true,
            "pw_force_bitperfect": false,
            "sync_audio_on_startup": false,
            "quality_fallback_behavior": "ask",
            "skip_sink_switch": false,
            "allow_quality_fallback": false
        }"#;

        let settings: AudioSettings =
            serde_json::from_str(legacy).expect("legacy JSON should deserialize");

        assert!(!settings.reserve_dac_while_running);
        assert!(settings.alsa_hardware_volume_controls.is_empty());
    }

    #[test]
    fn exact_alsa_mixer_controls_round_trip_by_route() {
        let (dir, store) = fresh_store("alsa-mixer-controls");
        let route_a: StableAlsaRouteKey =
            serde_json::from_str("\"alsa-route-v1|card=by-id:usb-D50-00|pcm=front|DEV=0\"")
                .expect("route A");
        let route_b: StableAlsaRouteKey =
            serde_json::from_str("\"alsa-route-v1|card=by-id:usb-D50-00|pcm=iec958|DEV=1\"")
                .expect("route B");
        let control_a = AlsaMixerControlId {
            name: "D50 III".to_string(),
            index: 0,
        };
        let control_b = AlsaMixerControlId {
            name: "Digital".to_string(),
            index: 1,
        };

        store
            .set_alsa_hardware_volume_control(route_a.clone(), control_a.clone())
            .expect("set route A");
        store
            .set_alsa_hardware_volume_control(route_b.clone(), control_b.clone())
            .expect("set route B");

        let settings = store.get_settings().expect("read settings");
        assert_eq!(
            settings.alsa_hardware_volume_controls.get(&route_a),
            Some(&control_a)
        );
        assert_eq!(
            settings.alsa_hardware_volume_controls.get(&route_b),
            Some(&control_b)
        );
        assert_ne!(route_a, route_b, "PCM DEV is part of the persisted key");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn database_without_mixer_control_map_migrates_to_empty() {
        let dir = unique_test_dir("legacy-alsa-mixer-column");
        {
            let store = AudioSettingsStore::new_at(&dir).expect("create current store");
            store
                .conn
                .execute(
                    "ALTER TABLE audio_settings DROP COLUMN alsa_hardware_volume_controls",
                    [],
                )
                .expect("simulate pre-map database");
        }

        let migrated = AudioSettingsStore::new_at(&dir).expect("migrate old store");
        assert!(migrated
            .get_settings()
            .expect("read migrated settings")
            .alsa_hardware_volume_controls
            .is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn reset_clears_route_scoped_alsa_mixer_controls() {
        let (dir, store) = fresh_store("reset-alsa-mixer-controls");
        let route: StableAlsaRouteKey =
            serde_json::from_str("\"alsa-route-v1|card=card:D50|pcm=hw|DEV=0\"").expect("route");
        store
            .enable_alsa_hardware_volume(
                route,
                AlsaMixerControlId {
                    name: "D50 III".to_string(),
                    index: 0,
                },
            )
            .expect("enable exact control");

        let reset = store.reset_all().expect("reset");
        assert!(!reset.alsa_hardware_volume);
        assert!(reset.alsa_hardware_volume_controls.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
