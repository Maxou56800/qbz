//! Settings > Audio + Playback controller — Slint-free port of
//! `crates/qbz/src/settings.rs` onto the SAME public backend stores:
//! `AudioSettingsStore` (get/set + `Player::reload_settings` /
//! `Player::reinit_device` apply — the audio backend is PROTECTED: only
//! these public calls), `PlaybackPreferencesStore`, the shared
//! `ui_prefs.json` (streaming quality), and the shared QConnect settings seam.
//!
//! Also owns device enumeration (`BackendManager::create_backend(type)
//! .enumerate_devices()` — public) with the Tauri ALSA section grouping,
//! and the cross-setting cascades from settings.rs (dac-passthrough,
//! streaming-only, backend switch).
//!
//! The Detected device limit row (#638 fix 3) IS wired (2026-08-17): the probe
//! and its cache moved out of the Slint binary crate into
//! `qbz_app::device_cap`, `refresh_device_cap` below owns the six explicit
//! triggers, and the row reads the cache off the snapshot.
//!
//! (The HiFi Wizard, the JACK banner and settings export/import were on this
//! list and are all shipped now — `qml/settings/DacWizardModal.qml`,
//! `AudioSettings.qml:70-78` and `settings_qt/devtools.rs`.)
//! - QConnect startup/device-name values use the transport module's one DB
//!   implementation; device-name edits also update the live service cache and
//!   take effect on the next connection, like upstream.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use cxx_qt_lib::QString;
use qbz_app::settings::network::NetworkSettingsState;
use qbz_app::settings::playback::{
    AutoplayMode, PlaybackPreferencesState, PlaybackPreferencesStore,
};
use qbz_app::shell::AppRuntime;
use qbz_audio::alsa_hardware_volume::{
    AlsaMixerControlId, HardwareVolumeCandidate, HardwareVolumeChoiceReason,
    HardwareVolumeDecision, HardwareVolumeProbe, HardwareVolumeProbeErrorKind,
    HardwareVolumeSelection, HardwareVolumeSelectionSource, HardwareVolumeSnapshot,
    HardwareVolumeUnsupportedReason, StableAlsaRouteKey,
};
use qbz_audio::backend::{AlsaPlugin, AudioBackendType, BackendManager};
use qbz_audio::settings::{AudioSettingsState, AudioSettingsStore};
use qbz_core::LoggingAdapter;
use serde::Serialize;

// Per-section controllers. Declared HERE (not in main.rs) so the settings
// controller can be split by concern without touching the app root: a
// `mod x;` in `settings_qt.rs` resolves to `src/settings_qt/x.rs`.
pub mod devtools;
pub mod import_export;
pub mod library;
pub mod network;
mod appearance_profile;
mod playback_storage;
pub mod offline;

// ---------------------------------------------------------------------------
// Stores (shared files with the Slint app)
// ---------------------------------------------------------------------------

static AUDIO: OnceLock<AudioSettingsState> = OnceLock::new();
static PLAYBACK: OnceLock<PlaybackPreferencesState> = OnceLock::new();
static NETWORK: OnceLock<NetworkSettingsState> = OnceLock::new();

fn audio() -> &'static AudioSettingsState {
    AUDIO.get_or_init(|| {
        AudioSettingsState::new().unwrap_or_else(|e| {
            log::warn!("[qbz-qt] audio settings store unavailable: {e}");
            AudioSettingsState::new_empty()
        })
    })
}

fn playback() -> &'static PlaybackPreferencesState {
    PLAYBACK.get_or_init(|| {
        PlaybackPreferencesState::new().unwrap_or_else(|e| {
            log::warn!("[qbz-qt] playback preferences store unavailable: {e}");
            PlaybackPreferencesState::new_empty()
        })
    })
}

/// Global, not per-user (see `qbz_app::settings::network`'s module docs):
/// read by the QConnect LAN and Cast kill switches in addition to the
/// future Network settings panel, so it must stay readable before any
/// account is active.
pub(crate) fn network() -> &'static NetworkSettingsState {
    NETWORK.get_or_init(|| {
        NetworkSettingsState::new().unwrap_or_else(|e| {
            log::warn!("[qbz-qt] network settings store unavailable: {e}");
            NetworkSettingsState::new_empty()
        })
    })
}

fn with_audio<T>(f: impl FnOnce(&AudioSettingsStore) -> Result<T, String>) -> Result<T, String> {
    let guard = audio()
        .store
        .lock()
        .map_err(|_| "audio store lock poisoned".to_string())?;
    let store = guard
        .as_ref()
        .ok_or_else(|| "audio settings store not open".to_string())?;
    f(store)
}

/// The live persisted audio settings, READ-ONLY (the audio path is
/// PROTECTED — nothing here writes or reinitialises a device).
///
/// Exposed so the now-playing stamp can re-derive its two output LEDs on the
/// TRACK / STREAM edge (`output_labels::publish_current`) without dragging in
/// `publish_snapshot`, which rebuilds the whole Settings document (device
/// enumeration + integrations) and is far too heavy for a track change.
pub fn audio_settings() -> qbz_audio::settings::AudioSettings {
    with_audio(|s| s.get_settings()).unwrap_or_default()
}

/// Remote control cannot assume a software mixer when settings are unavailable.
/// Keep this policy shared by capability advertisement and inbound execution.
pub fn allows_remote_volume() -> bool {
    with_audio(|store| store.get_settings())
        .map(|settings| !crate::output_labels::volume_locked(&settings))
        .unwrap_or(false)
}

/// Re-probe the local output device and refresh the #638 fix-3 cap cache.
///
/// **ORDERING — persist → refresh → publish, and Qt has a hazard Slint does
/// not.** The refresh must re-READ the settings (hence no arguments: it reads
/// the store itself), so every caller persists FIRST. It must also run
/// **before `apply_audio`**, not merely before the handler's closing
/// `publish_snapshot`: `apply_audio` ends with its own `publish_settings`
/// (see below), an earlier serialize that would ship the STALE summary and
/// win the race. The symptom of getting this wrong is "the row is right, but
/// only after you reopen Settings".
///
/// **The cache drop (contract D3), and its one hard-won exception.** A cap
/// change moves the effective request tier by exactly the mechanism a
/// streaming-quality change does, and the audio cache is quality-BLIND: local
/// play accepts any cached entry that is not below the request. So bytes
/// fetched before the cap must not keep serving, or the feature is observably
/// inert on every track already played this session.
///
/// The exception is the FIRST refresh of the process, and skipping it is not
/// an optimisation. `clear_audio_cache` is not an L1 drop — it reaches
/// `PlaybackCache::clear`, which unlinks every `<id>.audio` file on disk
/// (hundreds of MB to GBs). The cap cache starts empty, so at boot every
/// `None -> Some(tier)` reads as a change, and a naive comparison here wiped
/// the entire disk cache on EVERY launch with the toggle on, logging it as a
/// legitimate tier change. `device_cap` classifies the refresh instead
/// (`CapChange`), which also stops a superseded concurrent refresh from being
/// misattributed to this one.
///
/// The probe itself (`pw-dump` subprocess + `/proc/asound` reads) runs inside
/// `spawn_blocking` down in `device_cap`, and is an instant no-op while the
/// toggle is off — which is the default, so the common path costs nothing.
pub(crate) async fn refresh_device_cap(runtime: &Arc<AppRuntime<LoggingAdapter>>) {
    use qbz_app::device_cap::CapChange;
    let audio = audio_settings();
    let change = qbz_app::device_cap::refresh(
        audio.limit_quality_to_device,
        audio.output_device.clone(),
        // D7: enumerate the "System default" sink through the backend that is
        // actually playing, not always through PipeWire.
        audio.backend_type,
    )
    .await;
    match change {
        CapChange::Changed => {
            log::info!("[qbz-qt] device cap: request tier changed — clearing audio cache");
            runtime.core().player().clear_audio_cache();
        }
        CapChange::Seeded => {
            log::info!("[qbz-qt] device cap: seeded for this process — audio cache left intact");
        }
        CapChange::Unchanged => {}
    }
}

/// Whether `InfiniteRadio` autoplay is on — the queue footer's ∞ state and the
/// gate on the end-of-track refill engine. Exposed here (rather than reopening
/// the store from `queue_qt`) so both readers see the SAME process-wide handle:
/// two stores over one SQLite file would have the footer and the engine
/// disagreeing after a toggle until one of them re-read.
pub fn is_infinite_play() -> bool {
    with_playback(|s| s.get_preferences())
        .map(|p| p.autoplay_mode == AutoplayMode::InfiniteRadio)
        .unwrap_or(false)
}

/// Turn infinite play on/off. Off lands on `ContinueWithinSource` — the
/// reference's `toggle_infinite_play`, and the reason Settings' "Continue
/// playback" switch reads ON after infinite play is turned off here.
pub fn set_infinite_play(enabled: bool) {
    let mode = if enabled {
        AutoplayMode::InfiniteRadio
    } else {
        AutoplayMode::ContinueWithinSource
    };
    if let Err(e) = with_playback(|s| s.set_autoplay_mode(mode)) {
        log::error!("[qbz-qt] queue: set autoplay mode failed: {e}");
    }
}

fn with_playback<T>(
    f: impl FnOnce(&PlaybackPreferencesStore) -> Result<T, String>,
) -> Result<T, String> {
    let guard = playback()
        .store
        .lock()
        .map_err(|_| "playback preferences lock poisoned".to_string())?;
    let store = guard
        .as_ref()
        .ok_or_else(|| "playback preferences store not open".to_string())?;
    f(store)
}

// ---------------------------------------------------------------------------
// ui_prefs.json (streaming quality) — shared file, patched key-by-key so
// every OTHER Slint key survives.
// ---------------------------------------------------------------------------

/// Shared with `theme_qt.rs` (theme + theme_filter) so there is ONE spelling
/// of the path every reader and writer in the crate agrees on.
pub(crate) fn prefs_path() -> Option<std::path::PathBuf> {
    Some(dirs::data_dir()?.join("qbz").join("ui_prefs.json"))
}

// ---------------------------------------------------------------------------
// The shared-file write discipline — EVERY ui_prefs.json writer in this crate
// goes through `update_prefs` / `edit_prefs` / `save_pref`, no exceptions.
// Verify with `grep -rn 'fs::write\|json!({})' crates/qbz-qt/src/`: no hit may
// touch ui_prefs.json. (theme_qt.rs and search_qt.rs each carried a private
// copy of both anti-patterns until they were routed through here.)
//
// The same `read_json_object` + `write_json_object_atomic` pair covers the
// other documents this crate co-owns with the Slint app: `myqbz_branding.json`
// (below) and `locallibrary_ui.json` (local_state.rs).
//
// STILL TRUNCATING, in files this module does not own — same failure mode,
// smaller blast radius, listed so the next sweep has the list rather than a
// claim: recently_qt.rs (`recently_played.json`, Slint `recently.rs`),
// genre_filter_qt.rs (`genre_filter.json`, Slint `genre_filter.rs`),
// lyrics_qt.rs (`lyrics_prefs.json`, Slint `lyrics_prefs.rs`) and
// playlist_qt.rs (`playlist_orders.json`, Qt-only — races only itself).
//
// ui_prefs.json is co-owned with the SHIPPING Slint app and both processes do
// a whole-document read-modify-write, so two rules decide whether the file
// survives concurrency:
//
// 1. NEVER publish a partial file. `std::fs::write` opens O_TRUNC, so between
//    the truncate and the last byte the file on disk is short or empty. Slint's
//    `ui_prefs::load()` (crates/qbz/src/ui_prefs.rs:872-883) answers a parse
//    failure with `UiPrefs::default()`, and its next `save()` writes those
//    defaults back — one unlucky read inside our write window wipes theme,
//    npb_mode, streaming_quality, cast_quality_caps, renderer, the lot. Writing
//    a sibling temp file and `rename(2)`-ing it onto the target closes the
//    window: rename within one directory is atomic on Linux, so a concurrent
//    reader sees either the whole old document or the whole new one.
//    Window geometry is what made this urgent: it saves on every settled
//    resize, orders of magnitude more often than a settings toggle.
//
// 2. NEVER rebuild the document from scratch. The old fallback was
//    `unwrap_or_else(|| json!({}))`, which cannot tell "no file yet" (starting
//    from `{}` is correct) from "the file is there but did not parse" — and
//    that second case is exactly what Slint's own O_TRUNC window looks like
//    from over here. `{}` plus our key is a document containing ONLY our key:
//    the mirror image of the wipe above. A save we cannot do safely is SKIPPED
//    and logged, never guessed.
// ---------------------------------------------------------------------------

/// Read a JSON object for a read-modify-write.
///
/// `Some(map)` = parsed, and an ABSENT file yields an empty map (a first run
/// legitimately starts from `{}`). `None` = the path exists but is unreadable
/// or is not a JSON object — the caller must not write, or it would replace a
/// document it never managed to read.
pub(crate) fn read_json_object(
    path: &std::path::Path,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    match std::fs::read_to_string(path) {
        Ok(text) => match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(serde_json::Value::Object(map)) => Some(map),
            // Includes the empty string a mid-write O_TRUNC leaves behind.
            _ => {
                log::warn!(
                    "[qbz-qt] {} did not parse as a JSON object — skipping this \
                     write instead of rebuilding the document",
                    path.display()
                );
                None
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some(serde_json::Map::new()),
        Err(e) => {
            log::warn!(
                "[qbz-qt] {} unreadable ({e}) — skipping this write",
                path.display()
            );
            None
        }
    }
}

/// Publish a JSON object atomically: the whole document to a sibling temp file,
/// then `rename` onto the target. The pid in the temp name keeps two QBZ
/// processes from sharing one scratch file — each rename stays atomic on its
/// own, so the loser of a race is overwritten as a COMPLETE document rather
/// than merged byte-wise.
pub(crate) fn write_json_object_atomic(
    path: &std::path::Path,
    doc: &serde_json::Map<String, serde_json::Value>,
) {
    use std::io::Write as _;

    let Ok(text) = serde_json::to_string_pretty(doc) else {
        log::warn!("[qbz-qt] {} serialize failed — not writing", path.display());
        return;
    };
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            log::warn!(
                "[qbz-qt] {} parent directory unavailable ({e}) — not writing",
                path.display()
            );
            return;
        }
        sweep_stale_temps(parent);
    }
    // `<path>.<pid>.tmp`, built off the full path so it always lands in the
    // SAME directory: rename is only atomic within one filesystem.
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(format!(".{}.tmp", std::process::id()));
    let tmp = std::path::PathBuf::from(tmp);
    // create + write_all + sync_all rather than `fs::write`: the rename only
    // publishes the NAME, so without the fsync the bytes can still be sitting
    // in page cache when the box goes down and the reader that follows the
    // rename finds a zero-length file — the exact wipe the atomic publish
    // exists to prevent. ext4's `auto_da_alloc` covers this pattern in
    // practice, but it is a mount option, not a guarantee (and it is not the
    // only filesystem QBZ runs on).
    let written = std::fs::File::create(&tmp).and_then(|mut f| {
        f.write_all(text.as_bytes())?;
        f.sync_all()
    });
    if let Err(e) = written {
        log::warn!("[qbz-qt] {} temp write failed: {e}", tmp.display());
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    if let Err(e) = std::fs::rename(&tmp, path) {
        log::warn!("[qbz-qt] {} atomic rename failed: {e}", path.display());
        let _ = std::fs::remove_file(&tmp);
    }
}

/// A temp file this old cannot belong to a live write (they last
/// milliseconds); anything younger may be another QBZ process publishing
/// RIGHT NOW and is none of our business.
const STALE_TEMP_AGE: Duration = Duration::from_secs(3600);

/// Directories already swept this run — the sweep is one `read_dir` per
/// directory per process, not one per write.
static SWEPT_DIRS: Mutex<Vec<std::path::PathBuf>> = Mutex::new(Vec::new());

/// Drop `<name>.<pid>.tmp` leftovers. The publish above removes its own temp
/// on every failure path, but a SIGKILL between the write and the rename
/// leaves one behind for good — and a SIGKILL there is not hypothetical
/// locally: earlyoom shoots processes while a parallel Slint build eats the
/// box (CLAUDE.md "Build & memory").
///
/// Liveness is judged by AGE, not by the pid: `/proc/<pid>` would be
/// Linux-only, and a pid that has been recycled reads as alive anyway.
fn sweep_stale_temps(dir: &std::path::Path) {
    {
        let Ok(mut swept) = SWEPT_DIRS.lock() else {
            return;
        };
        if swept.iter().any(|d| d.as_path() == dir) {
            return;
        }
        swept.push(dir.to_path_buf());
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        // Only OUR naming: `<something>.<digits>.tmp`.
        let Some(stem) = name.strip_suffix(".tmp") else {
            continue;
        };
        let Some((_, pid)) = stem.rsplit_once('.') else {
            continue;
        };
        if pid.is_empty() || !pid.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .map(|t| t.elapsed().map(|age| age > STALE_TEMP_AGE).unwrap_or(false))
            .unwrap_or(false);
        if stale {
            match std::fs::remove_file(&path) {
                Ok(()) => log::info!("[qbz-qt] removed stale temp file {}", path.display()),
                Err(e) => log::warn!("[qbz-qt] stale temp {} not removed: {e}", path.display()),
            }
        }
    }
}

/// The ONE read-modify-write of ui_prefs.json, with a value carried OUT of the
/// closure. `edit` mutates the document and returns
/// `(anything-actually-changed, payload)` — a `false` skips the write entirely,
/// which is how the geometry dirty check keeps the many no-op resize events a
/// WM emits from each costing a rewrite of a file the whole app (and the Slint
/// build) shares.
///
/// The payload is what makes read-then-negate toggles safe: the OLD value has
/// to be read from the SAME document the new one is written into. Reading it
/// through `pref_bool` first would answer a torn read (Slint mid-write) with
/// the DEFAULT, and by the time this write ran the file would be readable
/// again — committing the negation of a value that was never on disk.
///
/// `None` = nothing was written, because the document could not be read
/// (`read_json_object` refuses rather than rebuilding). Callers must report the
/// pref UNCHANGED in that case, never the flip they asked for.
fn edit_prefs<T>(
    edit: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>) -> (bool, T),
) -> Option<T> {
    let path = prefs_path()?;
    let mut doc = read_json_object(&path)?;
    let (dirty, out) = edit(&mut doc);
    if dirty {
        write_json_object_atomic(&path, &doc);
    }
    Some(out)
}

/// `edit_prefs` for the writers that carry nothing out.
fn update_prefs(edit: impl FnOnce(&mut serde_json::Map<String, serde_json::Value>) -> bool) {
    let _ = edit_prefs(|doc| (edit(doc), ()));
}

/// Can this install actually deliver a toast?
///
/// Setting the process AUMID is necessary and NOT sufficient. An unpackaged
/// desktop app only receives toasts once Windows can RESOLVE that id, which it
/// does through either a Start-menu shortcut carrying
/// `System.AppUserModel.ID` or the `AppUserModelId` registry key. The MSI
/// writes both; a portable unzip has neither.
///
/// So the notifications row is offered on the strength of THIS, not of
/// `cfg!(windows)`. Advertising it unconditionally would put a switch in front
/// of portable users that can never produce a notification -- the "renders,
/// persists and drives nothing" failure the capability flags exist to prevent.
#[cfg(target_os = "windows")]
pub(crate) fn windows_toast_identity_registered() -> bool {
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_READ,
    };

    let key: Vec<u16> = "Software\\Classes\\AppUserModelId\\com.blitzfc.qbz\0"
        .encode_utf16()
        .collect();
    let mut handle: HKEY = std::ptr::null_mut();
    // SAFETY: `key` is NUL-terminated UTF-16 and outlives the call; `handle` is
    // a valid out-slot, only written on success. Read-only access.
    let rc = unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, key.as_ptr(), 0, KEY_READ, &mut handle) };
    if rc != 0 {
        return false;
    }
    // SAFETY: opened above and not used afterwards.
    unsafe {
        RegCloseKey(handle);
    }
    true
}

/// The Windows as-is disclaimer, kept as two fields (owner's call): the
/// "Don't show this again" box and the version it was ticked on.
pub(crate) const WINDOWS_DISCLAIMER_HIDDEN_KEY: &str = "windows_disclaimer_hidden";
pub(crate) const WINDOWS_DISCLAIMER_VERSION_KEY: &str = "windows_disclaimer_ack_version";

/// Both fields out of ONE parsed document.
///
/// Reading them with two `pref_*` calls would parse the file twice and could
/// synthesise a pair that never existed in a single snapshot -- the shared
/// `ui_prefs.json` has another writer (the Slint app) and this one is read at
/// construction, while it is most likely to be mid-write.
pub(crate) fn windows_disclaimer_state() -> (bool, String) {
    let Some(path) = prefs_path() else {
        return (false, String::new());
    };
    let Some(doc) = read_json_object(&path) else {
        return (false, String::new());
    };
    let hidden = doc
        .get(WINDOWS_DISCLAIMER_HIDDEN_KEY)
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let version = doc
        .get(WINDOWS_DISCLAIMER_VERSION_KEY)
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    (hidden, version)
}

/// Both fields into ONE transaction, and report whether it landed.
///
/// Two `save_pref` calls would be two whole-document read-modify-writes, and a
/// crash or a failed rename between them leaves a torn pair -- `hidden` with no
/// version, or a version with no `hidden`. Either half alone re-opens the
/// modal, so the user would tick the box and see it again.
pub(crate) fn save_windows_disclaimer_ack(version: &str) -> bool {
    edit_prefs(|doc| {
        doc.insert(
            WINDOWS_DISCLAIMER_HIDDEN_KEY.to_string(),
            serde_json::Value::Bool(true),
        );
        doc.insert(
            WINDOWS_DISCLAIMER_VERSION_KEY.to_string(),
            serde_json::Value::String(version.to_string()),
        );
        (true, ())
    })
    .is_some()
}

/// Flip a shared bool pref in ONE document — read and write inside the same
/// read-modify-write, never `pref_bool` then `save_pref`. `None` = the
/// document was unreadable, so nothing was written and nothing flipped.
pub fn toggle_pref_bool(key: &str, default: bool) -> Option<bool> {
    edit_prefs(|doc| {
        let next = !doc
            .get(key)
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(default);
        doc.insert(key.to_string(), serde_json::Value::Bool(next));
        (true, next)
    })
}

pub fn streaming_quality() -> String {
    let Some(path) = prefs_path() else {
        return "hires_plus".to_string();
    };
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|v| {
            v.get("streaming_quality")
                .and_then(|q| q.as_str().map(str::to_string))
        })
        .unwrap_or_else(|| "hires_plus".to_string())
}

fn save_streaming_quality(key: &str) {
    save_pref(
        "streaming_quality",
        serde_json::Value::String(key.to_string()),
    );
}

// ---------------------------------------------------------------------------
// ui_prefs.json (window chrome, phase 12) — the Slint `use_system_title_bar`
// pref (crates/qbz/src/ui_prefs.rs): SAME shared file, additive key patch so
// every other Slint key survives. Default TRUE on Linux (the Slint default
// is `!macos` — Linux keeps the system decorations). Applied at startup
// only: decorations negotiate at surface creation on Wayland, so a toggle
// takes effect on the next launch (restart semantics, 1:1 Slint).
// ---------------------------------------------------------------------------

/// PER-OS default, and it has to be: Linux keeps the system decorations,
/// macOS defaults to the overlay (custom) mode where the native traffic
/// lights float over the QBZ header.
///
/// This port hard-coded `true` on every platform. Because `ui_prefs.json` is
/// SHARED with the Slint build, that did not just break Qt's own macOS
/// chrome — once the key was written it also flipped the Slint binary to the
/// system title bar, since an explicit value overrides its per-OS default.
/// 1:1 with `crates/qbz/src/ui_prefs.rs:645-647`.
fn default_use_system_title_bar() -> bool {
    !cfg!(target_os = "macos")
}

pub fn use_system_title_bar() -> bool {
    let Some(path) = prefs_path() else {
        return default_use_system_title_bar();
    };
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|v| v.get("use_system_title_bar").and_then(|q| q.as_bool()))
        .unwrap_or_else(default_use_system_title_bar)
}

/// Flip + persist the pref; returns the new value (for the menu state).
///
/// The flip happens INSIDE the read-modify-write (`toggle_pref_bool`) — see
/// `edit_prefs`. When the document cannot be read nothing is written, and the
/// menu is told the pref is unchanged rather than being handed a flip the file
/// never got.
pub fn toggle_system_title_bar() -> bool {
    toggle_pref_bool("use_system_title_bar", default_use_system_title_bar()).unwrap_or_else(|| {
        let current = use_system_title_bar();
        log::warn!(
            "[qbz-qt] use_system_title_bar toggle skipped (prefs unreadable) — staying {current}"
        );
        current
    })
}

/// Hide the custom title bar entirely: no drawn controls and — the part that
/// is easy to miss — NO DRAG SURFACE. The reference reads it into
/// `chrome-drag-enabled` (`qbz-ui/ui/shell/HeaderBar.slint:594-596`), which
/// gates the press-to-move TouchArea and, through `chrome-controls`, the
/// drawn cluster as well. Only meaningful while the custom chrome is active;
/// the Appearance row disables itself under the system title bar.
///
/// Until 2026-08-04 this pref was written by the Settings row and read by
/// nobody in this port — the owner's "renders, persists, drives nothing".
pub fn hide_title_bar() -> bool {
    pref_bool("hide_title_bar", false)
}

/// Flip the two-finger swipe mapping (fingers right = back by default, the
/// natural-scrolling convention). For users running WITHOUT natural
/// scrolling — Slint `AppearanceState.invert-swipe-navigation`.
pub fn invert_swipe_navigation() -> bool {
    pref_bool("invert_swipe_navigation", false)
}

/// Put the playing track in the OS window title. The format is FIXED
/// ("{track} - {artist} | qbz", `qbz-ui/ui/app.slint:44`); the reference's
/// template row is commented out there and this port cut it too.
pub fn window_title_show() -> bool {
    pref_bool("window_title_show", false)
}

/// Draw the in-app min/max/close cluster at all (Slint
/// `AppearanceState.show-window-controls`, consumed at
/// `HeaderBar.slint:599`). "Disable if your window manager handles these."
pub fn show_window_controls() -> bool {
    pref_bool("show_window_controls", true)
}

/// Cluster on the LEFT instead of the right. The pref carries the STRING
/// ("left" | "right"); the settings document carries the index into
/// [`WC_POSITION_VALUES`], so the two representations meet here.
///
/// Left placement is not just a different x: the reference also flips the
/// button ORDER to the macOS one (close · max · min vs min · max · close —
/// `qbz-ui/ui/shell/WindowControls.slint:41-79`).
pub fn wc_on_left() -> bool {
    pref_str("wc_position", "right") == "left"
}

// ---------------------------------------------------------------------------
// ui_prefs.json (app-wide ambient background, phase 14) — the Slint
// `app_background` enum key: "off" | "ambient" | "blurred"
// (crates/qbz/src/ui_prefs.rs; default "off", the owner's store carries
// "ambient"). Same additive key patch.
// ---------------------------------------------------------------------------

/// Mode index, ui_prefs.rs `app_background_index` semantics, 1:1 with the
/// Slint: 0 = Off, 1 = Ambient (the album-triad metaball field), 2 = Blurred
/// art (the ImmersiveAtmosphere blurred-cover look). The two are DIFFERENT
/// looks and each has its own layer in AppShell.qml — an unknown key is Off.
pub fn app_background_mode() -> i32 {
    let Some(path) = prefs_path() else {
        return 0;
    };
    let key = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|v| {
            v.get("app_background")
                .and_then(|q| q.as_str().map(str::to_string))
        })
        .unwrap_or_else(|| "off".to_string());
    APP_BACKGROUND_VALUES
        .iter()
        .position(|v| *v == key)
        .unwrap_or(0) as i32
}

/// The Wallpaper mode's blur, in MultiEffect's own units: Qt documents
/// `blur` from 0.0 (no blur) to 1.0 (full blur), reached at the field's
/// `blurMax` of 64 (shell/WallpaperField.qml). 0.75 is the look the mode
/// shipped with and stays the default; Settings > Appearance edits it as
/// 0-100 %.
pub const WALLPAPER_BLUR_DEFAULT: f32 = 0.75;

pub fn wallpaper_blur() -> f32 {
    clamp_wallpaper_blur(pref_f32("wallpaper_blur", WALLPAPER_BLUR_DEFAULT))
}

/// Qt's range, whatever a hand-edited ui_prefs.json holds; NaN is the default.
fn clamp_wallpaper_blur(value: f32) -> f32 {
    if value.is_nan() {
        return WALLPAPER_BLUR_DEFAULT;
    }
    value.clamp(0.0, 1.0)
}

/// The Settings slider's 0-100 % step as the stored Qt value.
fn wallpaper_blur_from_percent(percent: i32) -> f32 {
    percent.clamp(0, 100) as f32 / 100.0
}

/// Live-tuning knobs (Slint AppearanceState defaults; the QBZ_BG_* envs are
/// the same dev knobs the Slint seeds at startup).
pub fn ambient_dim() -> f32 {
    std::env::var("QBZ_BG_DIM")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.35)
}
/// RETIRED 2026-08-13 (single-pulse redesign): VizSettle no longer owns a
/// driver timer — every whole-window animator ticks off the shared shell
/// pulse (`shell_pulse_ms` below / `QbzShell.pulseMs`). This knob still feeds
/// the `vizTickMs` qproperty, which nothing reads anymore; it stays only
/// because deleting a qproperty churns the bridge for no gain.
///
/// Was: VizSettle's interpolation tick in ms, QBZ_VIZ_TICK (default 16).
/// This was the single biggest GPU lever in the shell and it was NOT about
/// the spectrum: Qt Quick has no dirty-region rendering, so every tick of
/// that timer redrew the WHOLE window. Measured on the owner's 4070 at
/// half-screen with the Large band up — band alone 45-69%, band + ambient
/// 76-82%, ambient alone 36%. The band was the dominant term because it set
/// the redraw RATE, and the ambient is expensive because it turns the whole
/// scene into an all-alpha stack that then gets redrawn at that rate.
pub fn viz_tick_ms() -> i32 {
    std::env::var("QBZ_VIZ_TICK")
        .ok()
        .and_then(|v| v.parse::<i32>().ok())
        .map(|v| v.clamp(8, 100))
        .unwrap_or(16)
}

/// The shell repaint pulse period in ms, QBZ_PULSE_MS (default 33).
///
/// THE single-clock knob of the 2026-08-13 redesign: one Rust thread hops to
/// the Qt loop every `period` and bumps `QbzShell.pulseMs`, and every
/// continuous animator (the ambient background drift, VizSettle's frame
/// application) ticks off that ONE notify edge, so all of them dirty the
/// scene in the same event-loop turn and the window presents ONCE per
/// period — not once per animator. Qt Quick has no dirty-region rendering,
/// so presents/s IS the shell's GPU term; two unsynchronised ~30 Hz clocks
/// measured ~62 presents/s and 93-97% GPU on the owner's 4070.
///
/// 33 ms matches the FFT producer's TARGET_FPS = 30 (qbz-audio
/// visualizer/mod.rs:32): one pulse per published frame, so no published
/// frame is ever skipped and none is rendered twice. Clamped to [10, 200] —
/// below 10 ms you are paying presents the data cannot fill, above 200 ms
/// the motion reads as broken, not calm.
pub fn shell_pulse_ms() -> i32 {
    std::env::var("QBZ_PULSE_MS")
        .ok()
        .and_then(|v| v.parse::<i32>().ok())
        .map(|v| v.clamp(10, 200))
        .unwrap_or(33)
}

/// Cache the content pane in a texture layer, QBZ_PANE_LAYER (default off).
///
/// `layer.enabled` collapses the pane's whole subtree into ONE textured quad,
/// re-rendered only when something inside it changes. With the dynamic
/// background up every element in there is alpha-blended and unbatchable, so a
/// static track list currently costs its full draw list on every one of those
/// whole-window redraws. Off by default because it trades an FBO the size of
/// the pane, and because a SCROLLING list invalidates it every frame — it is
/// here to be measured, not assumed.
pub fn pane_layer() -> bool {
    matches!(
        std::env::var("QBZ_PANE_LAYER").as_deref(),
        Ok("1") | Ok("true")
    )
}

/// Offscreen render-target multiplier for the ambient field, QBZ_BG_SCALE.
///
/// RETIRED 2026-08-13: the field renders INLINE now (no FBO to size), and the
/// knob was measured to be a non-lever anyway (0.5 moved the GPU ~5 points in
/// mode 1 — the per-present cost, not the fragment cost, dominates). The
/// qproperty stays to avoid bridge churn; nothing reads it.
///
/// Was: 1.0 = the reference's own sizing, clamped to [0.2, 1.0] — below 0.2
/// the metaball lobes started to band on the upscale, above 1.0 there was
/// nothing to gain.
pub fn ambient_scale() -> f32 {
    std::env::var("QBZ_BG_SCALE")
        .ok()
        .and_then(|v| v.parse::<f32>().ok())
        .map(|v| v.clamp(0.2, 1.0))
        .unwrap_or(1.0)
}
pub fn ambient_surface_alpha() -> f32 {
    std::env::var("QBZ_BG_SURFACE_ALPHA")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.5)
}
pub fn ambient_bar_alpha() -> f32 {
    std::env::var("QBZ_BG_BAR_ALPHA")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.3)
}

/// Flip + persist the app-wide background off <-> on. Returns the new mode
/// index (0 = off, 1 = ambient, 2 = blurred art).
///
/// Turning it back ON restores the mode the user had PICKED in Appearance,
/// not a hardcoded "ambient": now that "Blurred art" is a distinct look, a
/// menu toggle that silently rewrote it to the metaball field would be a
/// setting the app changes behind the user's back. The previous mode rides
/// the additive `app_background_last` key (same one-document patch style as
/// every other pref here); absent or unusable, it falls back to "ambient".
///
/// Same one-document rule as `toggle_system_title_bar`: the current key is
/// read inside the write closure, so a torn read can no longer make the app
/// commit "ambient" over a user who had just turned it off (or the reverse).
/// Settings > Appearance > Background image > "Choose image…": the user's
/// own picture behind the wallpaper modes, outranking the desktop's.
pub fn pick_background_image() {
    crate::spawn(async {
        let Some(file) = rfd::AsyncFileDialog::new()
            .set_title(qbz_i18n::t("Background image"))
            .add_filter("Images", &["png", "jpg", "jpeg", "webp", "bmp", "avif"])
            .pick_file()
            .await
        else {
            return;
        };
        let path = file.path().to_string_lossy().to_string();
        save_pref("app_background_image", serde_json::json!(path));
        publish_snapshot().await;
        crate::wallpaper_qt::refresh();
    });
}

/// "System wallpaper": drop the user's own picture.
pub fn clear_background_image() {
    save_pref("app_background_image", serde_json::json!(""));
    crate::spawn(async {
        publish_snapshot().await;
        crate::wallpaper_qt::refresh();
    });
}

pub fn toggle_ambient_background() -> i32 {
    edit_prefs(|doc| {
        let current = doc
            .get("app_background")
            .and_then(|q| q.as_str())
            .unwrap_or("off")
            .to_string();
        if current == "off" {
            let restored = doc
                .get("app_background_last")
                .and_then(|q| q.as_str())
                .filter(|v| *v != "off" && APP_BACKGROUND_VALUES.contains(v))
                .unwrap_or("ambient")
                .to_string();
            let index = APP_BACKGROUND_VALUES
                .iter()
                .position(|v| *v == restored)
                .unwrap_or(1) as i32;
            doc.insert(
                "app_background".to_string(),
                serde_json::Value::String(restored),
            );
            (true, index)
        } else {
            doc.insert(
                "app_background_last".to_string(),
                serde_json::Value::String(current),
            );
            doc.insert(
                "app_background".to_string(),
                serde_json::Value::String("off".to_string()),
            );
            (true, 0)
        }
    })
    .unwrap_or_else(|| {
        let current = app_background_mode();
        log::warn!("[qbz-qt] app_background toggle skipped (prefs unreadable) — staying {current}");
        current
    })
}

// ---------------------------------------------------------------------------
// ui_prefs.json (now-playing bar mode, phase 18) — the Slint `npb_mode` key:
// "new" | "classic" | "small" | "large" (ui_prefs.rs:357-360; maps to
// ShellState.npb-mode 0/1/2/3). Same additive key patch.
// ---------------------------------------------------------------------------

/// The persisted mode as a bridge index (unknown keys fall back to "new").
pub fn npb_mode_index() -> i32 {
    let Some(path) = prefs_path() else {
        return 0;
    };
    let key = std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|v| {
            v.get("npb_mode")
                .and_then(|q| q.as_str().map(str::to_string))
        })
        .unwrap_or_else(|| "new".to_string());
    match key.as_str() {
        "classic" => 1,
        "small" => 2,
        "large" => 3,
        _ => 0,
    }
}

fn npb_mode_key(index: i32) -> &'static str {
    match index {
        1 => "classic",
        2 => "small",
        3 => "large",
        _ => "new",
    }
}

/// Persist a mode index (0-3) to the shared key; returns the index.
pub fn set_npb_mode(index: i32) -> i32 {
    let index = index.clamp(0, 3);
    save_pref(
        "npb_mode",
        serde_json::Value::String(npb_mode_key(index).to_string()),
    );
    index
}

// ---------------------------------------------------------------------------
// Shell chrome prefs — sidebar state + section-nav placement
// ---------------------------------------------------------------------------
// `sidebar_state` is the Slint ui_prefs key (ui_prefs.rs:473, default 0):
// 0 = open / 1 = mini / 2 = closed. Slint restores it at startup
// (main.rs:8742) and rewrites it from `ShellState.cycle-sidebar()`
// (persist-sidebar-state, main.rs:14398). The Qt port used to hardcode 0 on
// every launch, so a user who left the sidebar mini or closed got it back
// open — the "initial state is wrong" divergence.

/// The persisted three-state sidebar (clamped 0-2; default 0 = open).
pub fn sidebar_state() -> i32 {
    let Some(path) = prefs_path() else {
        return 0;
    };
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|v| v.get("sidebar_state").and_then(serde_json::Value::as_i64))
        .map(|s| (s as i32).clamp(0, 2))
        .unwrap_or(0)
}

/// Persist the sidebar state (clamped 0-2); returns the stored value.
pub fn set_sidebar_state(state: i32) -> i32 {
    let state = state.clamp(0, 2);
    save_pref("sidebar_state", serde_json::Value::Number(state.into()));
    state
}

/// Persisted open-sidebar width (#771), clamped to the panel's range;
/// absent = the pre-#771 240.
pub fn sidebar_width() -> i32 {
    crate::panel_resize::clamp_sidebar_width(pref_i32(
        "sidebar_width",
        crate::panel_resize::SIDEBAR_MIN_WIDTH,
    ))
}

pub fn set_sidebar_width(width: i32) -> i32 {
    let width = crate::panel_resize::clamp_sidebar_width(width);
    save_pref("sidebar_width", serde_json::Value::Number(width.into()));
    width
}

/// Persisted queue/lyrics column width (#771), clamped; absent = 300.
pub fn queue_panel_width() -> i32 {
    crate::panel_resize::clamp_queue_width(pref_i32(
        "queue_panel_width",
        crate::panel_resize::QUEUE_MIN_WIDTH,
    ))
}

pub fn set_queue_panel_width(width: i32) -> i32 {
    let width = crate::panel_resize::clamp_queue_width(width);
    save_pref("queue_panel_width", serde_json::Value::Number(width.into()));
    width
}

/// Section-nav placement defaults — the Slint `ShellState` literals
/// (state.slint:4124 / :4129). Sidebar ON, compact-header OFF.
pub const NAV_IN_SIDEBAR_DEFAULT: bool = true;
pub const NAV_HEADER_COMPACT_DEFAULT: bool = false;

/// Sections live in the sidebar (ON) or in the header (OFF).
pub fn nav_in_sidebar() -> bool {
    pref_bool("nav_in_sidebar", NAV_IN_SIDEBAR_DEFAULT)
}

/// While the nav is in the HEADER: use the icon-only compact form even when
/// the sidebar is not fully closed. No effect while `nav_in_sidebar` is ON.
pub fn nav_header_compact() -> bool {
    pref_bool("nav_header_compact", NAV_HEADER_COMPACT_DEFAULT)
}

/// The collapsible "My QBZ" sidebar section (opt-in, OFF by default). When ON it
/// reveals the three sub-toggles below; a fresh profile keeps the flat entry.
pub fn collapsible_myqbz() -> bool {
    pref_bool("collapsible_myqbz", false)
}
pub fn myqbz_show_mixtapes() -> bool {
    pref_bool("myqbz_show_mixtapes", true)
}
pub fn myqbz_show_collections() -> bool {
    pref_bool("myqbz_show_collections", true)
}
pub fn hide_standard_myqbz() -> bool {
    pref_bool("hide_standard_myqbz", false)
}

// Sidebar tree collapse state — persisted so a collapsed section stays
// collapsed across restarts (owner request 2026-09-10). All default EXPANDED
// (false). The playlist tree and the three My QBZ sections each keep their own.
pub fn sidebar_playlists_collapsed() -> bool {
    pref_bool("sidebar_playlists_collapsed", false)
}
pub fn myqbz_collapsed() -> bool {
    pref_bool("myqbz_collapsed", false)
}
pub fn myqbz_mixtapes_collapsed() -> bool {
    pref_bool("myqbz_mixtapes_collapsed", false)
}
pub fn myqbz_collections_collapsed() -> bool {
    pref_bool("myqbz_collections_collapsed", false)
}

/// Playlist rows draw a 2x2 micro-collage of track covers (Slint
/// SidebarState.playlist-collage). Opt-OUT — default ON.
pub fn sidebar_playlist_collage() -> bool {
    pref_bool("sidebar_playlist_collage", true)
}

// ---------------------------------------------------------------------------
// Large-NPB dock prefs (the cover's eye toggle + the band's mode cycle)
// ---------------------------------------------------------------------------

/// Whether the Large dock shows its FFT band. Defaults ON (the Slint dock's
/// `large-visualizer-on` default) — the eye button is always visible while it
/// is off, so an accidental hide is one click from being undone.
pub fn large_visualizer_on() -> bool {
    let Some(path) = prefs_path() else {
        return true;
    };
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|v| {
            v.get("large_visualizer_on")
                .and_then(serde_json::Value::as_bool)
        })
        .unwrap_or(true)
}

/// Persist the band visibility; returns the stored value.
pub fn set_large_visualizer_on(on: bool) -> bool {
    save_pref("large_visualizer_on", serde_json::Value::Bool(on));
    on
}

pub fn seekbar_waveform() -> bool {
    pref_bool("seekbar_waveform", false)
}

/// The band's render mode: 0 Bars / 1 Waveform / 2 Energy / 3 Goniometer /
/// 4 Oscilloscope.
pub fn large_spectrum_mode() -> i32 {
    let Some(path) = prefs_path() else {
        return 0;
    };
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|v| {
            v.get("large_spectrum_mode")
                .and_then(serde_json::Value::as_i64)
        })
        .map(|m| (m as i32).clamp(0, 4))
        .unwrap_or(0)
}

/// Resolve a stored Large-NPB mode against the renderer that actually won.
///
/// Goniometer and Oscilloscope are native scene-graph line strips. Qt's
/// Software/Null backend can still draw their QML guide axes, but cannot draw
/// the trace; exposing either mode there leaves a convincing-looking empty
/// instrument. Keep the stored preference untouched so returning to a GPU
/// restores it, and use Bars as the live fallback on the no-GPU tier.
pub fn large_spectrum_mode_for_tier(mode: i32, gpu_tier: bool) -> i32 {
    let mode = mode.clamp(0, 4);
    if gpu_tier || mode < 3 {
        mode
    } else {
        0
    }
}

/// Persist the band mode (clamped 0-4); returns the stored index.
pub fn set_large_spectrum_mode(mode: i32) -> i32 {
    let mode = mode.clamp(0, 4);
    save_pref(
        "large_spectrum_mode",
        serde_json::Value::Number(mode.into()),
    );
    mode
}

/// The "Show track playing context" pref (Playback settings; feeds the
/// SongCard layers icon — SettingsState.show-context-icon).
pub fn show_context_icon() -> bool {
    with_playback(|s| s.get_preferences().map(|p| p.show_context_icon)).unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Main-window geometry — the Slint `window_width` / `window_height` /
// `window_maximized` keys (crates/qbz/src/ui_prefs.rs:567-585, restored in
// crates/qbz/src/main.rs:8211-8282, written by the winit Resized handler at
// main.rs:1399-1435). SAME shared file, SAME types: the sizes are JSON floats
// holding LOGICAL (device-independent) pixels, the flag is a bool. Both
// frontends read each other's numbers, so a type or unit change here silently
// corrupts the shared profile — and "unit" includes the interface-size
// preset. Both frontends now bake it into their native high-DPI scale.
//
// Why this exists at all: Main.qml used to hardcode 1280x800 and never save,
// so the Qt build opened one responsive tier below the Slint build on the same
// machine (1280 < 1366 flips the now-playing-bar side fraction 0.30 -> 0.39,
// which caps the Classic song card at ~446px instead of 560 and elides titles
// ~114px early). The tier logic was never wrong — the window was.
//
// NOT carried: `window_x` / `window_y`. Slint stores them as PHYSICAL outer
// coordinates (main.rs:8271 feeds a `PhysicalPosition`) while QML's
// `Window.x/y` are device-independent, so writing ours into that key would
// displace the Slint window by the DPR on any HiDPI display. Position restore
// is a no-op on Wayland for both toolkits anyway (main.rs:8209), and the
// owner's store still holds the `i32::MIN` never-saved sentinel — leaving the
// two keys untouched is exactly what Slint already sees.
// ---------------------------------------------------------------------------

/// The app's floor, straight from Slint: `app.slint:52-53` declares
/// `min-width: 940px / UiScale.factor` and the restore clamp repeats it at
/// `main.rs:8214-8215`.
///
/// The constants are the preset-free physical floor. [`window_min_size`]
/// converts them to Qt's scaled logical coordinate system at startup.
pub const WINDOW_MIN_WIDTH: f32 = 940.0;
pub const WINDOW_MIN_HEIGHT: f32 = 600.0;

/// The interface-size preset factor for a persisted `ui_scale` slug — the SAME
/// table as Slint's `ui_prefs::ui_scale_factor` (ui_prefs.rs:243-251).
///
/// Qt receives this factor before `QGuiApplication` is constructed, through
/// `QT_SCALE_FACTOR`. It therefore uses the same scaled-logical coordinate
/// system as the persisted geometry: text, vector graphics and controls are
/// laid out and rasterized at the effective DPR instead of enlarging a
/// finished scene texture.
fn ui_scale_factor_for(slug: &str) -> f32 {
    match slug {
        "xs" => 0.8,
        "small" => 0.9,
        "large" => 1.2,
        "xl" => 1.5,
        _ => 1.0,
    }
}

/// The persisted interface-size multiplier consumed before Qt starts.
pub fn ui_scale_factor() -> f32 {
    ui_scale_factor_for(&pref_str("ui_scale", "default"))
}

/// The factor Qt is actually using. Normally `main` writes the persisted
/// preset into `QT_SCALE_FACTOR`, so this equals [`ui_scale_factor`]. Keeping
/// the explicit-environment case exact matters for developer DPI tests: the
/// shared geometry must not be rewritten in the override's units.
fn qt_effective_scale_factor() -> f32 {
    std::env::var("QT_SCALE_FACTOR")
        .ok()
        .and_then(|value| value.trim().parse::<f32>().ok())
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or_else(ui_scale_factor)
}

/// The app floor in Qt's native high-DPI logical coordinates. Multiplying the
/// result by the interface factor returns the same 940x600 baseline on every
/// preset, matching the reference frontend's `min-width / UiScale.factor`.
pub fn window_min_size() -> (f32, f32) {
    let factor = qt_effective_scale_factor();
    (WINDOW_MIN_WIDTH / factor, WINDOW_MIN_HEIGHT / factor)
}

/// Change the scale slug while keeping the persisted native window footprint
/// stable. Stored geometry is expressed in Slint scaled-logical units, so a
/// slug change without this rebase would make the next launch multiply the old
/// units by the new factor and unexpectedly resize the window.
fn set_ui_scale_in_doc(doc: &mut serde_json::Map<String, serde_json::Value>, slug: &str) -> bool {
    let old_slug = doc
        .get("ui_scale")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("default");
    if old_slug == slug {
        return false;
    }
    let ratio = ui_scale_factor_for(old_slug) as f64 / ui_scale_factor_for(slug) as f64;
    for key in ["window_width", "window_height"] {
        if let Some(value) = doc.get(key).and_then(serde_json::Value::as_f64) {
            if value.is_finite() && value > 0.0 {
                doc.insert(key.to_string(), serde_json::Value::from(value * ratio));
            }
        }
    }
    doc.insert(
        "ui_scale".to_string(),
        serde_json::Value::String(slug.to_string()),
    );
    true
}

fn save_ui_scale(slug: &str) {
    update_prefs(|doc| set_ui_scale_in_doc(doc, slug));
}

/// The never-saved size. 1180x760 is both `app.slint:47-48`'s preferred size
/// (what Slint falls back to by doing nothing) and the literal Slint plugs in
/// at `main.rs:8223-8224` when it has to size the window itself. One number
/// either way — and emphatically not the old 1280x800.
pub const WINDOW_DEFAULT_WIDTH: f32 = 1180.0;
pub const WINDOW_DEFAULT_HEIGHT: f32 = 760.0;

/// Restored floating size in Qt high-DPI logical pixels. Qt and the persisted
/// document now use the same interface-size-aware coordinate system, so no
/// second conversion is allowed here.
///
/// The all-or-nothing gate is Slint's `has_saved_size` (main.rs:8220-8221):
/// BOTH axes must clear the minimum or the pair counts as never saved. A
/// half-written profile (one key present, one missing or absurd) therefore
/// opens at the default instead of a 0-wide window.
pub fn window_size() -> (f32, f32) {
    let doc = prefs_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok());
    let axis = |key: &str| -> f32 {
        doc.as_ref()
            .and_then(|v| v.get(key))
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0) as f32
    };
    let factor = ui_scale_factor_for(
        doc.as_ref()
            .and_then(|v| v.get("ui_scale"))
            .and_then(|v| v.as_str())
            .unwrap_or("default"),
    );
    // The ACCEPTANCE test runs on the STORED value, not the converted one, and
    // it is asymmetric — because Slint's is. Slint gates on
    // `stored >= min_logical_w.min(940.0)` with `min_logical_w = 940 / factor`
    // (main.rs:8214-8221), so the `.min()` pins the threshold at 940 for every
    // preset at or below 1.0 and only relaxes it for presets above 1.0.
    // Testing `stored * factor >= 940` instead — which reads like the same
    // thing — is STRICTER on the small presets: at `xs` (0.8, which the kiosk
    // image pins) it demands a stored 1175 where Slint accepts 940. A profile
    // holding 1000x640 was therefore declared never-saved, opened at the
    // default, and then had 1180/0.8 = 1475 written back over the user's stored
    // size — in the file the Slint build reads.
    let (stored_w, stored_h) = (axis("window_width"), axis("window_height"));
    let floor_w = (WINDOW_MIN_WIDTH / factor).min(WINDOW_MIN_WIDTH);
    let floor_h = (WINDOW_MIN_HEIGHT / factor).min(WINDOW_MIN_HEIGHT);
    if stored_w >= floor_w && stored_h >= floor_h {
        let qt_factor = qt_effective_scale_factor();
        (stored_w * factor / qt_factor, stored_h * factor / qt_factor)
    } else {
        (WINDOW_DEFAULT_WIDTH, WINDOW_DEFAULT_HEIGHT)
    }
}

/// Last maximized state. Slint only ever RE-APPLIES a true here
/// (main.rs:8276) — a false leaves the fresh window in its natural state.
pub fn window_maximized() -> bool {
    pref_bool("window_maximized", false)
}

/// Persist the settled geometry — the Slint `WindowEvent::Resized` handler
/// (main.rs:1399-1435) rule for rule:
///
/// - `window_width`/`window_height` hold the FLOATING size ONLY. A maximized
///   or fullscreen frame must never overwrite them, or the next launch would
///   reproduce the maximized footprint as a floating window (Slint #618).
/// - Frames below the app minimum are ignored: a minimize reports 0x0 and
///   mid-transition frames undershoot.
/// - The >0.5px dirty check (main.rs:1426-1429) plus the maximized-flag
///   comparison decide whether the file is rewritten at all. The many no-op
///   resize events a WM emits must not each cost a read-modify-write of a
///   file the whole app shares.
///
/// Single ATOMIC read-modify-write over the WHOLE document (`update_prefs`):
/// every other Slint key survives untouched, and the file is never visible in
/// a half-written state to the Slint process reading it.
pub fn save_window_geometry(width: f32, height: f32, maximized: bool, fullscreen: bool) {
    // A non-finite frame never reaches the file. `serde_json::Value::from`
    // maps NaN/inf to `Value::Null` (there is no JSON spelling for either), and
    // Slint declares `#[serde(default)] pub window_width: f32` — `default`
    // covers a MISSING field, NOT an explicit null. A null there fails the
    // WHOLE `UiPrefs` deserialization, so Slint would fall back to its defaults
    // and flatten the shared document on its next save. Cheapest possible
    // guard for the worst possible outcome.
    if !width.is_finite() || !height.is_finite() {
        log::warn!("[qbz-qt] ignoring non-finite window geometry {width}x{height}");
        return;
    }
    update_prefs(|doc| {
        // Qt normally receives this same persisted factor before
        // QGuiApplication starts. An explicit QT_SCALE_FACTOR is allowed for
        // diagnostics, so convert between the two only in that override case.
        let factor = ui_scale_factor_for(
            doc.get("ui_scale")
                .and_then(|v| v.as_str())
                .unwrap_or("default"),
        );
        let qt_factor = qt_effective_scale_factor();
        let stored_width = width * qt_factor / factor;
        let stored_height = height * qt_factor / factor;

        // Read the three previous values first — the dirty comparison decides
        // whether the document is touched at all, and it compares stored
        // against stored.
        let was_maximized = doc
            .get("window_maximized")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let prev_width = doc
            .get("window_width")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0) as f32;
        let prev_height = doc
            .get("window_height")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(0.0) as f32;

        // The minimum is checked in Qt's scaled logical pixels.
        let size_dirty = !maximized
            && !fullscreen
            && width >= WINDOW_MIN_WIDTH / qt_factor
            && height >= WINDOW_MIN_HEIGHT / qt_factor
            && ((prev_width - stored_width).abs() > 0.5
                || (prev_height - stored_height).abs() > 0.5);
        if !size_dirty && was_maximized == maximized {
            return false;
        }

        doc.insert(
            "window_maximized".to_string(),
            serde_json::Value::Bool(maximized),
        );
        if size_dirty {
            doc.insert(
                "window_width".to_string(),
                serde_json::Value::from(stored_width as f64),
            );
            doc.insert(
                "window_height".to_string(),
                serde_json::Value::from(stored_height as f64),
            );
        }
        true
    });
}

// ---------------------------------------------------------------------------
// Appearance + Integrations prefs (phase 19) — generic ui_prefs.json
// accessors (SAME additive patch discipline) + the per-user stores the
// Appearance/Integrations panels read (tray, My-QBZ branding).
// ---------------------------------------------------------------------------

pub fn pref_bool(key: &str, default: bool) -> bool {
    let Some(path) = prefs_path() else {
        return default;
    };
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|v| v.get(key).and_then(|q| q.as_bool()))
        .unwrap_or(default)
}

pub fn pref_str(key: &str, default: &str) -> String {
    let Some(path) = prefs_path() else {
        return default.to_string();
    };
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|v| v.get(key).and_then(|q| q.as_str().map(str::to_string)))
        .unwrap_or_else(|| default.to_string())
}

/// i32 reader — same additive single-key discipline as `pref_bool`/`pref_str`.
/// Needed for keys the SLINT app writes as JSON numbers (its typed ui_prefs
/// struct declares them i32, e.g. the immersive remember-last triple), which
/// `pref_str` (string-only) cannot see.
pub fn pref_i32(key: &str, default: i32) -> i32 {
    let Some(path) = prefs_path() else {
        return default;
    };
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|v| v.get(key).and_then(serde_json::Value::as_i64))
        .map(|n| n as i32)
        .unwrap_or(default)
}

/// Whole-value reader — same additive single-key discipline as
/// `pref_bool`/`pref_str`/`pref_i32`, for keys whose value is a NESTED
/// document rather than a scalar. Added for the hotkeys layer's `keybindings`
/// map (2026-08-03 hotkeys-port contract §3.2): a JSON object of action id ->
/// shortcut, co-owned with the Slint app, which `hotkeys_qt` reads whole and
/// writes back through the same `save_pref` single-key patch.
pub fn pref_json(key: &str) -> Option<serde_json::Value> {
    let path = prefs_path()?;
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|v| v.get(key).cloned())
}

// ---------------------------------------------------------------------------
// Local Library tab order + landing tab
// ---------------------------------------------------------------------------

/// The shipped Local Library order. The first entry is also the landing tab
/// for a fresh Local Library mount and for an unauthenticated offline entry.
///
/// This preference is deliberately APP-WIDE (`ui_prefs.json`), not per-user:
/// "Start offline" must resolve it before a Qobuz account exists. Unknown,
/// duplicated and missing ids are normalized so a stale/future document can
/// never make a tab disappear or leave the view without a valid landing.
pub(crate) const LOCAL_TAB_DEFAULT_ORDER: &[&str] =
    &["genres", "albums", "artists", "folders", "tracks"];

/// Settings order is user-facing and therefore stable: Top remains index 0
/// and the default, followed by the two sidebar positions and Bottom.
const LOCAL_GENRE_FILTER_POSITION_VALUES: &[&str] = &["top", "right", "left", "bottom"];

fn local_genre_filters_position_from(raw: &str) -> String {
    LOCAL_GENRE_FILTER_POSITION_VALUES
        .iter()
        .copied()
        .find(|value| *value == raw)
        .unwrap_or("top")
        .to_string()
}

pub(crate) fn local_genre_filters_position() -> String {
    local_genre_filters_position_from(&pref_str("local_genre_filters_position", "top"))
}

fn normalize_local_tab_order(value: Option<&serde_json::Value>) -> Vec<String> {
    let mut order = Vec::with_capacity(LOCAL_TAB_DEFAULT_ORDER.len());
    if let Some(serde_json::Value::Array(items)) = value {
        for id in items.iter().filter_map(serde_json::Value::as_str) {
            if LOCAL_TAB_DEFAULT_ORDER.contains(&id) && !order.iter().any(|v| v == id) {
                order.push(id.to_string());
            }
        }
    }
    for id in LOCAL_TAB_DEFAULT_ORDER {
        if !order.iter().any(|v| v == id) {
            order.push((*id).to_string());
        }
    }
    order
}

pub(crate) fn local_tab_order() -> Vec<String> {
    let value = pref_json("local_tab_order");
    normalize_local_tab_order(value.as_ref())
}

/// Fresh Kiosk entries always land on Albums. Explicit history/landing tabs
/// bypass this resolver; desktop continues to respect the user's order.
pub(crate) fn local_landing_tab(kiosk: bool) -> String {
    local_landing_tab_from(&local_tab_order(), kiosk)
}

fn local_landing_tab_from(order: &[String], kiosk: bool) -> String {
    if kiosk {
        return "albums".to_string();
    }
    order
        .first()
        .cloned()
        .unwrap_or_else(|| "albums".to_string())
}

/// Construction-time seed for `QbzBridge.settingsJson`. NavFlyout and Local
/// Library can be built before the asynchronous full settings snapshot lands;
/// seeding this one boot-critical key prevents a default-order flash and makes
/// the logged-off landing deterministic on the first frame.
pub(crate) fn settings_seed_json() -> String {
    serde_json::json!({ "localTabOrder": local_tab_order() }).to_string()
}

fn save_local_tab_order_payload(raw: &str) -> bool {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return false;
    };
    if !value.is_array() {
        return false;
    }
    let order = normalize_local_tab_order(Some(&value));
    save_pref("local_tab_order", serde_json::json!(order));
    true
}

/// f32 reader WITH a default — same additive single-key discipline as
/// `pref_bool`/`pref_str`/`pref_i32`. Added for the miniplayer geometry keys
/// (`mini_width`/`mini_height`), which the Slint app declares as f32 and
/// therefore writes as JSON numbers (`crates/qbz/src/ui_prefs.rs:556-559`).
/// `read_pref_f32` below returns an `Option` and has no default, so folding a
/// default into each call site would be the third private copy of one read the
/// accessor block exists to prevent (see that function's own comment).
pub fn pref_f32(key: &str, default: f32) -> f32 {
    let Some(path) = prefs_path() else {
        return default;
    };
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|v| v.get(key).and_then(serde_json::Value::as_f64))
        .map(|n| n as f32)
        .unwrap_or(default)
}

/// Read one ui_prefs key as an f32. The crate had no public reader — theme_qt
/// kept a private one — so the volume restore would otherwise have grown a
/// third private copy of the same read (the exact duplication the write
/// discipline below exists to prevent).
pub(crate) fn read_pref_f32(key: &str) -> Option<f32> {
    let path = prefs_path()?;
    let text = std::fs::read_to_string(path).ok()?;
    let doc: serde_json::Value = serde_json::from_str(&text).ok()?;
    doc.get(key)?.as_f64().map(|v| v as f32)
}

/// One key of ui_prefs.json, as raw JSON. `None` when the file, the key or
/// the parse is missing — every caller treats that as "not set".
pub(crate) fn read_pref(key: &str) -> Option<serde_json::Value> {
    let path = prefs_path()?;
    let text = std::fs::read_to_string(path).ok()?;
    let doc: serde_json::Value = serde_json::from_str(&text).ok()?;
    doc.get(key).cloned()
}

/// Additive single-key patch of ui_prefs.json — THE writer every other pref
/// setter in this file funnels through, so they all inherit `update_prefs`'
/// atomic rename and its refusal to rebuild an unparsable document.
pub fn save_pref(key: &str, value: serde_json::Value) {
    update_prefs(|doc| {
        if appearance_profile::managed(key) && doc.get(key) != Some(&value) {
            doc.insert("appearance_profile".into(), serde_json::json!("custom"));
        }
        doc.insert(key.to_string(), value);
        true
    });
}

/// Persist Preferred GPU as one atomic pair: the readable model name retained
/// for older builds plus the stable Vulkan identity used by the Qt launcher.
/// The process-local Qt index is deliberately never written.
pub(crate) fn save_gpu_preference(gpu: Option<&crate::renderer_qt::GpuInfo>) {
    let (name, identity) = gpu
        .map(|gpu| (gpu.name.as_str(), gpu.identity.as_str()))
        .unwrap_or(("auto", ""));
    update_prefs(|doc| {
        doc.insert("gpu_power".to_string(), serde_json::json!(name));
        doc.insert("gpu_identity".to_string(), serde_json::json!(identity));
        true
    });
}

// Tray store (qbz_app::settings::tray — per-user tray_settings.db, the SAME
// file the Slint tray_settings.rs glue writes).
static TRAY: OnceLock<qbz_app::settings::tray::TraySettingsState> = OnceLock::new();

/// `pub(crate)` for the tray port (contract A-36): `on_session_entered` reads
/// `enable_tray` + `tray_icon_theme` here to build `tray_qt::init`'s two
/// arguments, and `tray_bridge`'s seed reads `close_to_tray`. Every caller was
/// inside this module until then. **No signature change, no `pub`, no
/// re-export** — and duplicating the accessor instead would be the
/// third-private-copy this block exists to prevent.
///
/// Binds ONCE per process and does not rebind on a user switch — a deliberate,
/// labelled divergence (§13-D20, ticket T-1): `USER_DIR` is itself a
/// whole-shell `OnceLock` that also feeds the pinned store and discover prefs,
/// so a tray-only rebind would fix one of three and hide the other two.
pub(crate) fn tray() -> &'static qbz_app::settings::tray::TraySettingsState {
    TRAY.get_or_init(|| {
        let state = qbz_app::settings::tray::TraySettingsState::new_empty();
        if let Some(dir) = crate::sidebar_qt::user_dir() {
            if let Err(e) = state.init_at(&dir) {
                log::warn!("[qbz-qt] tray settings store unavailable: {e}");
            }
        }
        state
    })
}

// My-QBZ branding (myqbz_prefs.rs — per-user myqbz_branding.json; the label
// row in LIBRARY & VISUALS).
fn myqbz_label() -> String {
    crate::sidebar_qt::user_dir()
        .map(|d| d.join("myqbz_branding.json"))
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|v| v.get("label").and_then(|q| q.as_str().map(str::to_string)))
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "My QBZ".to_string())
}

/// myqbz_branding.json is shared with the Slint app the same way ui_prefs.json
/// is (crates/qbz/src/myqbz_prefs.rs), so it gets the same treatment: atomic
/// publish, and an unparsable document is left alone instead of rebuilt.
fn save_myqbz_label(label: &str) {
    let Some(path) = crate::sidebar_qt::user_dir().map(|d| d.join("myqbz_branding.json")) else {
        return;
    };
    let Some(mut doc) = read_json_object(&path) else {
        return;
    };
    // Trimmed-empty coerces to the default, it does NOT store "".
    // `myqbz_prefs.rs:121-128` does the same, and it matters both ways: the
    // sidebar/nav-flyout row would otherwise render a blank label with no way
    // to get the name back except retyping it, and the READ side
    // (`myqbz_label()` above) already filters empty to "My QBZ" — so storing
    // "" would leave the file and the UI disagreeing.
    let trimmed = label.trim();
    let stored = if trimmed.is_empty() {
        crate::myqbz_prefs_qt::DEFAULT_LABEL
    } else {
        trimmed
    };
    doc.insert(
        "label".to_string(),
        serde_json::Value::String(stored.to_string()),
    );
    write_json_object_atomic(&path, &doc);
    // The label is not read from this document at render time — the sidebar
    // and the nav flyout bind `QbzMyQbz.brandingJson`, which is seeded once at
    // construction. Without this republish the row keeps the old name until
    // the next launch.
    crate::myqbz_prefs_qt::republish_branding();
}

pub const STREAMING_QUALITY_KEYS: &[&str] = &["mp3", "cd", "hires", "hires_plus"];
pub const STREAMING_QUALITY_LABELS: &[&str] = &["MP3", "CD Quality", "Hi-Res", "Hi-Res+"];

/// Target loudness presets (label msgid, LUFS). Index 1 is the store default.
pub(crate) const NORMALIZATION_TARGETS: &[(&str, f32)] = &[
    ("-11 LUFS · Loud", -11.0),
    ("-14 LUFS · Streaming (default)", -14.0),
    ("-16 LUFS · Apple Music", -16.0),
    ("-18 LUFS · ReplayGain", -18.0),
    ("-23 LUFS · Broadcast (EBU R128)", -23.0),
];

fn normalization_target_index(lufs: f32) -> Option<usize> {
    NORMALIZATION_TARGETS
        .iter()
        .position(|(_, v)| (v - lufs).abs() < 0.01)
}

// ---------------------------------------------------------------------------
// Snapshot
// ---------------------------------------------------------------------------

const DSD_MODE_LABELS: &[&str] = &[
    "Convert to PCM (works everywhere)",
    "DoP — DSD over PCM (bit-perfect)",
    "Native DSD (kernel support required)",
];
const DSD_MODE_VALUES: &[&str] = &["convert", "dop", "native"];
const ALSA_PLUGIN_LABELS: &[&str] = &[
    "hw (Direct Hardware)",
    "plughw (Auto-convert)",
    "pcm (Most compatible)",
];
const ALSA_PLUGIN_VALUES: &[AlsaPlugin] = &[AlsaPlugin::Hw, AlsaPlugin::PlugHw, AlsaPlugin::Pcm];
const RETRY_BEHAVIOR_LABELS: &[&str] =
    &["Ask me", "Always try lowest quality", "Always skip track"];
const RETRY_BEHAVIOR_VALUES: &[&str] = &["ask", "always_fallback", "always_skip"];
const QCONNECT_STARTUP_LABELS: &[&str] = &["Remember state", "On by default", "Off by default"];
const QCONNECT_STARTUP_VALUES: &[&str] = &["remember_last", "on", "off"];
const QCONNECT_CONFLICT_POLICY_LABELS: &[&str] = &[
    "Ask every time",
    "Continue on the active renderer",
    "Continue here with the Qobuz Connect queue",
    "Continue local playback and replace the Qobuz Connect queue",
];
// Appearance option tables (AppearanceSettings.slint / ui_prefs.rs).
// 3 and 4 paint the desktop wallpaper (wallpaper_qt.rs): behind the window,
// or through the Blurred-art atmosphere pass.
const APP_BACKGROUND_LABELS: &[&str] = &["Off", "Ambient", "Blurred art", "Wallpaper", "Wallpaper, blurred"];
const APP_BACKGROUND_VALUES: &[&str] = &["off", "ambient", "blurred", "wallpaper", "wallpaper-blurred"];
const LANGUAGE_LABELS: &[&str] = &[
    "Auto",
    "English",
    "Español",
    "Français",
    "Deutsch",
    "Português",
    "Русский",
    "日本語",
    "Nederlands",
];
const LANGUAGE_VALUES: &[&str] = &["auto", "en", "es", "fr", "de", "pt", "ru", "ja", "nl"];
const UI_SCALE_LABELS: &[&str] = &["Extra small", "Small", "Default", "Large", "Extra large"];
const UI_SCALE_VALUES: &[&str] = &["xs", "small", "default", "large", "xl"];
/// The app-wide typeface (Settings > Appearance > Typography & Language).
///
/// The SAME five the lyrics panel offers (`lyrics_qt::FONTS`) and the same
/// slugs. The four named families are bundled in `qml/assets/fonts/`, so an
/// explicit choice cannot fail on a machine that lacks the face. "System" is
/// the exception: it deliberately leaves Qt's operating-system choice alone.
///
/// Only "System" is translated — a typeface name is a proper noun.
const APP_FONT_LABELS: &[&str] = &[
    "System",
    "LINE Seed JP",
    "Montserrat",
    "Noto Sans",
    "Source Sans 3",
];
const APP_FONT_VALUES: &[&str] = &[
    "system",
    "line-seed-jp",
    "montserrat",
    "noto-sans",
    "source-sans-3",
];

/// The family names the slugs resolve to, as the font files register them.
/// Index-aligned with [`APP_FONT_VALUES`]; "system" maps to the empty string.
const APP_FONT_FAMILIES: &[&str] = &[
    "",
    "LINE Seed JP",
    "Montserrat",
    "Noto Sans",
    "Source Sans 3",
];

/// The persisted app-font choice as an index into [`APP_FONT_VALUES`].
pub fn app_font_index() -> i32 {
    index_of(APP_FONT_VALUES, &pref_str("app_font", "system"), 0)
}

/// The family the persisted choice resolves to, or "" for "System".
///
/// "System" means the font Qt would pick on its own, i.e. exactly what this
/// app rendered before the setting existed. It deliberately does NOT mean
/// Inter: the bundled Inter has only ever reached Qt Quick CONTROLS (see
/// qml/FontPreload.qml), so making "System" mean Inter would change every
/// label in the app for people who never touched the setting.
pub fn app_font_family() -> String {
    APP_FONT_FAMILIES
        .get(app_font_index() as usize)
        .unwrap_or(&"")
        .to_string()
}
const IMMERSIVE_SEARCH_LABELS: &[&str] = &[
    "Disabled",
    "Replace current queue",
    "Play next",
    "Add to queue",
];
const IMMERSIVE_SEARCH_VALUES: &[&str] = &["disabled", "replace", "next", "queue"];
const IMMERSIVE_VIEW_LABELS: &[&str] = &[
    "Remember last",
    "Album Reactive",
    "Static",
    "Coverflow",
    "Spectrum",
    "Lyrics",
    "Queue",
];
const IMMERSIVE_VIEW_VALUES: &[&str] = &[
    "remember",
    "reactive",
    "static",
    "coverflow",
    "spectrum",
    "lyrics",
    "queue",
];
const MINI_VIEW_LABELS: &[&str] = &[
    "Remember last used",
    "Micro",
    "Compact",
    "Artwork",
    "Queue",
    "Lyrics",
];
const MINI_VIEW_VALUES: &[&str] = &["remember", "micro", "compact", "artwork", "queue", "lyrics"];
const STARTUP_PAGE_LABELS: &[&str] = &["Home", "Where you left off"];
const STARTUP_PAGE_VALUES: &[&str] = &["home", "remember"];
const WC_POSITION_LABELS: &[&str] = &["Left", "Right"];
const WC_POSITION_VALUES: &[&str] = &["left", "right"];
const RENDERER_LABELS: &[&str] = &[
    "Auto (recommended)",
    "GPU",
    "GPU (compatibility)",
    "Software",
];
const RENDERER_VALUES: &[&str] = &["auto", "wgpu", "gl", "software"];
const TRAY_ICON_LABELS: &[&str] = &["Auto", "Mono light", "Mono dark", "Color"];
const TRAY_ICON_VALUES: &[&str] = &["auto", "mono-light", "mono-dark", "color"];
const AUTO_THEME_SOURCE_LABELS: &[&str] = &["System Colors", "Wallpaper Sync", "Custom Image"];
const AUTO_THEME_SOURCE_VALUES: &[&str] = &["system", "wallpaper", "image"];

fn index_of(values: &[&str], key: &str, default: usize) -> i32 {
    values.iter().position(|v| *v == key).unwrap_or(default) as i32
}

/// Preferred-GPU dropdown. CLASS-LEVEL — Auto / integrated / discrete — which
/// is the ceiling of what any of these platforms offers (Qt: "No further
/// adapter configurability is provided at this time"; Slint's wgpu path is
/// class-level too, its documented F7 limitation).
///
/// Until 2026-08-11 this was Auto + whatever happened to be persisted, and the
/// select arm silently DROPPED every index but 0 — so the control could not
/// select a GPU and did not say so. PARITY-DEBT #83.
///
/// The values are `renderer_qt::GPU_POWER_VALUES`, the same class keys the
/// shipping Slint build parses out of the SHARED ui_prefs.json.
fn gpu_power_choice() -> (Vec<String>, i32) {
    // REAL devices, enumerated with Vulkan — never classes. Offering
    // "Discrete GPU" as a fixed row is what let you select hardware the
    // machine does not have; this lists what is actually present, by model,
    // in the same shape as the reference (`main.rs:7427-7438`).
    //
    // A box with no Vulkan (no libvulkan, no ICD) enumerates nothing and gets
    // Auto alone — the honest answer, since no GPU choice could be applied
    // there either.
    let gpus = crate::renderer_qt::gpus();
    let mut opts = vec![qbz_i18n::t("Auto (recommended)")];
    opts.extend(gpus.iter().map(|g| g.label()));
    // Index 0 is Auto; UI positions are independent of Qt's process-local
    // Vulkan indices (which can contain gaps when a CPU adapter is filtered).
    let index = crate::renderer_qt::resolve_saved_gpu()
        .and_then(|selected| {
            gpus.iter()
                .position(|gpu| gpu.identity == selected.identity)
        })
        .map(|position| position as i32 + 1)
        .unwrap_or(0);
    (opts, index)
}

#[derive(Clone, Default, Serialize)]
pub struct DeviceOption {
    pub label: String,
    pub bp: bool,
    pub group: String,
}

#[derive(Clone, Default, Serialize)]
pub struct HardwareVolumeOption {
    pub label: String,
    pub detail: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum HardwareVolumeUiStatus {
    #[default]
    Disabled,
    Probing,
    Selected,
    Ambiguous,
    Stale,
    PermissionDenied,
    DeviceBusy,
    DeviceUnavailable,
    NoMixer,
    Unsupported,
    WriteFailed,
}

impl HardwareVolumeUiStatus {
    fn key(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Probing => "probing",
            Self::Selected => "selected",
            Self::Ambiguous => "ambiguous",
            Self::Stale => "stale",
            Self::PermissionDenied => "permission_denied",
            Self::DeviceBusy => "device_busy",
            Self::DeviceUnavailable => "device_unavailable",
            Self::NoMixer => "no_mixer",
            Self::Unsupported => "unsupported",
            Self::WriteFailed => "write_failed",
        }
    }

    fn message(self) -> String {
        match self {
            Self::Disabled | Self::Selected => String::new(),
            Self::Probing => qbz_i18n::t("Checking ALSA mixer controls…"),
            Self::Ambiguous => qbz_i18n::t(
                "This device exposes multiple playback controls. Choose which one QBZ should use.",
            ),
            Self::Stale => qbz_i18n::t(
                "The saved mixer control is no longer available. Choose another control.",
            ),
            Self::PermissionDenied => {
                qbz_i18n::t("QBZ does not have permission to read this device's ALSA mixer.")
            }
            Self::DeviceBusy => {
                qbz_i18n::t("The ALSA mixer is busy. Close the app using it and try again.")
            }
            Self::DeviceUnavailable => {
                qbz_i18n::t("The selected ALSA device is no longer available.")
            }
            Self::NoMixer => qbz_i18n::t("This ALSA device does not expose a mixer."),
            Self::Unsupported => qbz_i18n::t(
                "This ALSA device has no compatible hardware volume control. Direct playback remains fixed at 100%.",
            ),
            Self::WriteFailed => qbz_i18n::t(
                "QBZ could not write the selected ALSA mixer control. Direct playback remains fixed at 100%.",
            ),
        }
    }
}

#[derive(Clone, Default)]
struct HardwareVolumeUiState {
    device_id: Option<String>,
    route_key: Option<StableAlsaRouteKey>,
    candidates: Vec<HardwareVolumeCandidate>,
    selected: Option<AlsaMixerControlId>,
    status: HardwareVolumeUiStatus,
}

#[derive(Default)]
struct HardwareVolumeUiSnapshot {
    options: Vec<HardwareVolumeOption>,
    selected_index: i32,
    selected: String,
    status: &'static str,
    message: String,
    can_change: bool,
    choice_token: String,
}

static HARDWARE_VOLUME_UI: Mutex<HardwareVolumeUiState> = Mutex::new(HardwareVolumeUiState {
    device_id: None,
    route_key: None,
    candidates: Vec::new(),
    selected: None,
    status: HardwareVolumeUiStatus::Disabled,
});

fn hardware_volume_candidate_detail(candidate: &HardwareVolumeCandidate) -> String {
    let range = if let Some(range) = candidate.db_range {
        format!(
            "{:.2}–{:.2} dB",
            range.min as f64 / 100.0,
            range.max as f64 / 100.0
        )
    } else if let Some(range) = candidate.raw_range {
        format!("{}–{}", range.min, range.max)
    } else {
        String::new()
    };
    let channels = candidate.channels.join(", ");
    match (candidate.recommended, range.is_empty(), channels.is_empty()) {
        (true, false, false) => format!("★ {range} · {channels}"),
        (true, false, true) => format!("★ {range}"),
        (true, true, false) => format!("★ {channels}"),
        (true, true, true) => "★".to_string(),
        (false, false, false) => format!("{range} · {channels}"),
        (false, false, true) => range,
        (false, true, false) => channels,
        (false, true, true) => String::new(),
    }
}

fn hardware_volume_ui_snapshot(
    audio: &qbz_audio::settings::AudioSettings,
) -> HardwareVolumeUiSnapshot {
    let Ok(state) = HARDWARE_VOLUME_UI.lock() else {
        return HardwareVolumeUiSnapshot::default();
    };
    if state.device_id.as_deref() != audio.output_device.as_deref() {
        return HardwareVolumeUiSnapshot {
            status: if audio.alsa_hardware_volume {
                HardwareVolumeUiStatus::Probing.key()
            } else {
                HardwareVolumeUiStatus::Disabled.key()
            },
            message: if audio.alsa_hardware_volume {
                HardwareVolumeUiStatus::Probing.message()
            } else {
                String::new()
            },
            ..Default::default()
        };
    }

    let options = state
        .candidates
        .iter()
        .map(|candidate| HardwareVolumeOption {
            label: candidate.id.to_string(),
            detail: hardware_volume_candidate_detail(candidate),
        })
        .collect::<Vec<_>>();
    let selected_index = state
        .selected
        .as_ref()
        .and_then(|selected| {
            state
                .candidates
                .iter()
                .position(|candidate| candidate.id == *selected)
        })
        .map(|index| index as i32)
        .unwrap_or(-1);
    let choice_token = if matches!(
        state.status,
        HardwareVolumeUiStatus::Ambiguous | HardwareVolumeUiStatus::Stale
    ) {
        format!(
            "{}|{}",
            state
                .route_key
                .as_ref()
                .map(StableAlsaRouteKey::as_str)
                .unwrap_or_default(),
            state
                .candidates
                .iter()
                .map(|candidate| candidate.id.to_string())
                .collect::<Vec<_>>()
                .join("|")
        )
    } else {
        String::new()
    };

    HardwareVolumeUiSnapshot {
        selected: state
            .selected
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default(),
        status: state.status.key(),
        message: state.status.message(),
        can_change: state.candidates.len() > 1 && state.selected.is_some(),
        choice_token,
        options,
        selected_index,
    }
}

#[derive(Default, Serialize)]
pub struct SettingsDoc {
    // Audio
    #[serde(rename = "streamingQualities")]
    pub streaming_qualities: Vec<String>,
    #[serde(rename = "streamingQualityIndex")]
    pub streaming_quality_index: i32,
    pub backends: Vec<String>,
    #[serde(rename = "backendIndex")]
    pub backend_index: i32,
    #[serde(rename = "backendIsAlsa")]
    pub backend_is_alsa: bool,
    /// True when the WASAPI exclusive backend is the active one. AudioSettings
    /// QML gates the Exclusive-mode and DSD rows on it - see the comment there
    /// for why they are hidden rather than disabled.
    #[serde(rename = "backendIsWasapi")]
    pub backend_is_wasapi: bool,
    /// True on macOS when the active backend is System default, i.e. the
    /// CoreAudio path that honours `exclusive_mode` through Hog Mode
    /// (PR #391). AudioSettings QML shows and enables the Exclusive-mode row
    /// on it (#748). Never true off macOS, so `=== true` gates stay inert.
    #[serde(rename = "backendIsCoreAudio")]
    pub backend_is_coreaudio: bool,
    #[serde(rename = "backendIsPipewire")]
    pub backend_is_pipewire: bool,
    #[serde(rename = "backendIsJack")]
    pub backend_is_jack: bool,
    pub devices: Vec<DeviceOption>,
    #[serde(rename = "deviceIndex")]
    pub device_index: i32,
    #[serde(rename = "alsaPlugins")]
    pub alsa_plugins: Vec<String>,
    #[serde(rename = "alsaPluginIndex")]
    pub alsa_plugin_index: i32,
    #[serde(rename = "alsaPluginIsHw")]
    pub alsa_plugin_is_hw: bool,
    #[serde(rename = "alsaDirectSelected")]
    pub alsa_direct_selected: bool,
    #[serde(rename = "alsaHardwareVolume")]
    pub alsa_hardware_volume: bool,
    #[serde(rename = "alsaHardwareVolumeOptions")]
    pub alsa_hardware_volume_options: Vec<HardwareVolumeOption>,
    #[serde(rename = "alsaHardwareVolumeIndex")]
    pub alsa_hardware_volume_index: i32,
    #[serde(rename = "alsaHardwareVolumeSelected")]
    pub alsa_hardware_volume_selected: String,
    #[serde(rename = "alsaHardwareVolumeStatus")]
    pub alsa_hardware_volume_status: &'static str,
    #[serde(rename = "alsaHardwareVolumeMessage")]
    pub alsa_hardware_volume_message: String,
    #[serde(rename = "alsaHardwareVolumeCanChange")]
    pub alsa_hardware_volume_can_change: bool,
    #[serde(rename = "alsaHardwareVolumeChoiceToken")]
    pub alsa_hardware_volume_choice_token: String,
    #[serde(rename = "dsdModes")]
    pub dsd_modes: Vec<String>,
    #[serde(rename = "dsdModeIndex")]
    pub dsd_mode_index: i32,
    #[serde(rename = "limitQualityToDevice")]
    pub limit_quality_to_device: bool,
    /// #638 fix 3 — the composed "Detected device limit" value, e.g.
    /// `"192 kHz · Hi-Res+"`. EMPTY means no cap is active, and the row hides
    /// on that rather than rendering a blank value.
    #[serde(rename = "deviceCapSummary")]
    pub device_cap_summary: String,
    /// False = the probe fell back to the common rate set, so the row shows
    /// its caveat. True while no cap is active, which keeps that caveat from
    /// flashing before the first refresh lands.
    #[serde(rename = "deviceCapDetected")]
    pub device_cap_detected: bool,
    #[serde(rename = "exclusiveMode")]
    pub exclusive_mode: bool,
    #[serde(rename = "reserveDac")]
    pub reserve_dac: bool,
    #[serde(rename = "dacPassthrough")]
    pub dac_passthrough: bool,
    #[serde(rename = "pwForceBitperfect")]
    pub pw_force_bitperfect: bool,
    #[serde(rename = "allowQualityFallback")]
    pub allow_quality_fallback: bool,
    #[serde(rename = "syncAudioOnStartup")]
    pub sync_audio_on_startup: bool,
    #[serde(rename = "skipSinkSwitch")]
    pub skip_sink_switch: bool,
    // Playback
    #[serde(rename = "continuePlayback")]
    pub continue_playback: bool,
    #[serde(rename = "showContextIcon")]
    pub show_context_icon: bool,
    pub gapless: bool,
    /// Settable via settingsBool("normalization", …) but never published
    /// back, so both now-playing bars had to shadow the toggle locally.
    pub normalization: bool,
    /// Target loudness presets (translated labels) + the selected index; a
    /// value outside the presets (qbzd CLI, an import) shows as a trailing
    /// "Custom (N LUFS)" entry rather than being silently re-mapped.
    #[serde(rename = "normalizationTargets")]
    pub normalization_targets: Vec<String>,
    #[serde(rename = "normalizationTargetIndex")]
    pub normalization_target_index: i32,
    #[serde(rename = "normalizationPreventClipping")]
    pub normalization_prevent_clipping: bool,
    #[serde(rename = "persistSession")]
    pub persist_session: bool,
    #[serde(rename = "resumePosition")]
    pub resume_position: bool,
    #[serde(rename = "streamUncached")]
    pub stream_uncached: bool,
    #[serde(rename = "bufferSeconds")]
    pub buffer_seconds: i32,
    #[serde(rename = "streamingOnly")]
    pub streaming_only: bool,
    #[serde(rename = "playbackCache")]
    pub playback_cache: qbz_models::playback_cache::PlaybackCacheSettings,
    #[serde(rename = "playbackMemoryProfile")]
    pub playback_memory_profile: String,
    #[serde(rename = "appearanceProfile")]
    pub appearance_profile: String,
    #[serde(rename = "playbackMemoryApplyBusy")]
    pub playback_memory_apply_busy: bool,
    #[serde(rename = "playbackStorage")]
    pub playback_storage: playback_storage::Snapshot,
    #[serde(rename = "playbackCacheUsage")]
    pub playback_cache_usage: Option<qbz_cache::CacheStats>,
    #[serde(rename = "retryBehaviors")]
    pub retry_behaviors: Vec<String>,
    #[serde(rename = "retryBehaviorIndex")]
    pub retry_behavior_index: i32,
    #[serde(rename = "qconnectStartupModes")]
    pub qconnect_startup_modes: Vec<String>,
    #[serde(rename = "qconnectStartupIndex")]
    pub qconnect_startup_index: i32,
    #[serde(rename = "qconnectConflictPolicies")]
    pub qconnect_conflict_policies: Vec<String>,
    #[serde(rename = "qconnectConflictPolicyIndex")]
    pub qconnect_conflict_policy_index: i32,
    #[serde(rename = "qconnectDeviceName")]
    pub qconnect_device_name: String,
    #[serde(rename = "qconnectDeviceNameDefault")]
    pub qconnect_device_name_default: String,
    // Appearance (phase 19; the ui_prefs defaults mirror ui_prefs.rs)
    #[serde(rename = "albumHeaderGradient")]
    pub album_header_gradient: bool,
    #[serde(rename = "compactAlbumHeader")]
    pub compact_album_header: bool,
    #[serde(rename = "appBackgroundModes")]
    pub app_background_modes: Vec<String>,
    #[serde(rename = "appBackgroundIndex")]
    pub app_background_index: i32,
    /// The user's own background image ("" = the desktop wallpaper).
    #[serde(rename = "appBackgroundImage")]
    pub app_background_image: String,
    /// Persisted fieldset state of the Appearance > Theme groups (default
    /// expanded): the Custom theme editor, the Auto theme rows, and the
    /// options of the selected dynamic background.
    #[serde(rename = "themeCustomCollapsed")]
    pub theme_custom_collapsed: bool,
    #[serde(rename = "themeAutoCollapsed")]
    pub theme_auto_collapsed: bool,
    #[serde(rename = "ambientOptionsCollapsed")]
    pub ambient_options_collapsed: bool,
    #[serde(rename = "autoThemeSources")]
    pub auto_theme_sources: Vec<String>,
    #[serde(rename = "autoThemeSourceIndex")]
    pub auto_theme_source_index: i32,
    /// The picked image for `auto_theme_source == "image"`, or "". Only the
    /// LABEL of the "Select Image..." row (AppearanceSettings.slint:284-286);
    /// `theme_qt::auto_source` reads the pref itself.
    #[serde(rename = "autoThemeImagePath")]
    pub auto_theme_image_path: String,
    /// "KDE Plasma" / "GNOME" / … — the hint row's subject. Empty means the
    /// detector could not name one, and the row hides (`:294`).
    #[serde(rename = "autoThemeDetectedDe")]
    pub auto_theme_detected_de: String,
    #[serde(rename = "intelligentSearch")]
    pub intelligent_search: bool,
    pub languages: Vec<String>,
    #[serde(rename = "languageIndex")]
    pub language_index: i32,
    #[serde(rename = "uiScales")]
    pub ui_scales: Vec<String>,
    #[serde(rename = "uiScaleIndex")]
    pub ui_scale_index: i32,
    #[serde(rename = "appFonts")]
    pub app_fonts: Vec<String>,
    #[serde(rename = "appFontIndex")]
    pub app_font_index: i32,
    #[serde(rename = "immersiveSearchActions")]
    pub immersive_search_actions: Vec<String>,
    #[serde(rename = "immersiveSearchActionIndex")]
    pub immersive_search_action_index: i32,
    #[serde(rename = "immersiveDefaultViews")]
    pub immersive_default_views: Vec<String>,
    #[serde(rename = "immersiveDefaultViewIndex")]
    pub immersive_default_view_index: i32,
    #[serde(rename = "navInSidebar")]
    pub nav_in_sidebar: bool,
    #[serde(rename = "navHeaderCompact")]
    pub nav_header_compact: bool,
    #[serde(rename = "collapsibleMyqbz")]
    pub collapsible_myqbz: bool,
    #[serde(rename = "myqbzShowMixtapes")]
    pub myqbz_show_mixtapes: bool,
    #[serde(rename = "myqbzShowCollections")]
    pub myqbz_show_collections: bool,
    #[serde(rename = "hideStandardMyqbz")]
    pub hide_standard_myqbz: bool,
    // Persisted sidebar-tree collapse state (default expanded).
    #[serde(rename = "sidebarPlaylistsCollapsed")]
    pub sidebar_playlists_collapsed: bool,
    #[serde(rename = "myqbzCollapsed")]
    pub myqbz_collapsed: bool,
    #[serde(rename = "myqbzMixtapesCollapsed")]
    pub myqbz_mixtapes_collapsed: bool,
    #[serde(rename = "myqbzCollectionsCollapsed")]
    pub myqbz_collections_collapsed: bool,
    #[serde(rename = "myQbzLabel")]
    pub my_qbz_label: String,
    #[serde(rename = "sidebarPlaylistCollage")]
    pub sidebar_playlist_collage: bool,
    #[serde(rename = "queueTrackArtwork")]
    pub queue_track_artwork: bool,
    #[serde(rename = "libraryTrackArtwork")]
    pub library_track_artwork: bool,
    #[serde(rename = "localLibraryTrackArtwork")]
    pub local_library_track_artwork: bool,
    #[serde(rename = "showFeaturedArtists")]
    pub show_featured_artists: bool,
    #[serde(rename = "playIndicatorAnimation")]
    pub play_indicator_animation: bool,
    #[serde(rename = "seekbarWaveform")]
    pub seekbar_waveform: bool,
    #[serde(rename = "invertSwipeNavigation")]
    pub invert_swipe_navigation: bool,
    #[serde(rename = "inAppToasts")]
    pub in_app_toasts: bool,
    #[serde(rename = "systemNotifications")]
    pub system_notifications: bool,
    #[serde(rename = "windowTitleShow")]
    pub window_title_show: bool,
    #[serde(rename = "useSystemTitleBar")]
    pub use_system_title_bar: bool,
    #[serde(rename = "hideTitleBar")]
    pub hide_title_bar: bool,
    #[serde(rename = "wcPositions")]
    pub wc_positions: Vec<String>,
    #[serde(rename = "wcPositionIndex")]
    pub wc_position_index: i32,
    #[serde(rename = "showWindowControls")]
    pub show_window_controls: bool,
    #[serde(rename = "showSkipTen")]
    pub show_skip_ten: bool,
    #[serde(rename = "showVolumeSteppers")]
    pub show_volume_steppers: bool,
    #[serde(rename = "miniDefaultViews")]
    pub mini_default_views: Vec<String>,
    #[serde(rename = "miniDefaultViewIndex")]
    pub mini_default_view_index: i32,
    #[serde(rename = "startupPages")]
    pub startup_pages: Vec<String>,
    #[serde(rename = "startupPageIndex")]
    pub startup_page_index: i32,
    #[serde(rename = "showPurchases")]
    pub show_purchases: bool,
    /// Opt-in: a click on a top-level section row (Discover / Library / Local
    /// Library / My QBZ) also NAVIGATES, landing on that section's first entry.
    /// Off by default, which is the behaviour that shipped: a click only opens
    /// the section's flyout and the user picks a tab from it.
    #[serde(rename = "navClickFirstTab")]
    pub nav_click_first_tab: bool,
    /// Authoritative Local Library tab order. First = default landing.
    #[serde(rename = "localTabOrder")]
    pub local_tab_order: Vec<String>,
    /// Where the Genres three-stage browser sits around its album results.
    #[serde(rename = "genreFiltersPosition")]
    pub genre_filters_position: String,
    pub renderers: Vec<String>,
    #[serde(rename = "rendererIndex")]
    pub renderer_index: i32,
    #[serde(rename = "gpuPowers")]
    pub gpu_powers: Vec<String>,
    #[serde(rename = "gpuPowerIndex")]
    pub gpu_power_index: i32,
    /// Can this platform honour a Preferred-GPU choice at all? False on macOS,
    /// where QRhi hardcodes the system default device — the row HIDES there
    /// rather than offering a control that cannot do anything.
    #[serde(rename = "gpuSelectable")]
    pub gpu_selectable: bool,
    // Appearance > SYSTEM TRAY (tray_settings.db)
    #[serde(rename = "trayEnable")]
    pub tray_enable: bool,
    #[serde(rename = "trayCloseToTray")]
    pub tray_close_to_tray: bool,
    /// STORAGE ONLY — there is no Settings row and no reader (owner ruling K5).
    /// The Slint sheet documents why the reference hides the toggle rather than
    /// showing it disabled (`crates/qbz-ui/ui/settings/AppearanceSettings.slint:897-903`):
    /// redirecting the window-manager minimize button to the tray means owning
    /// that button, and Wayland forbids a client from intercepting the
    /// compositor's minimize.
    ///
    /// **Nothing in QML writes this key** (`grep -rn 'tray-minimize-to-tray' qml/`
    /// → 0), and that is the point of K5, not an oversight: the field carries
    /// the value into the settings DOCUMENT so a reader can see it, and the
    /// write arm exists so the key becomes reachable the day a row is added
    /// without a second edit. It is NOT protecting the value from loss — the
    /// per-user store is written by `qbz-app`, not by this document, so a Qt
    /// session could not drop what the Slint build wrote in any case.
    #[serde(rename = "trayMinimizeToTray")]
    pub tray_minimize_to_tray: bool,
    /// macOS only: while closed to the menu bar, switch the activation policy to
    /// `.accessory` (no Dock icon). Off keeps the Dock icon, Spotify-style. The
    /// row is `visible: isMacos`, so on Linux it renders nothing at all.
    #[serde(rename = "trayMacHideDock")]
    pub tray_mac_hide_dock: bool,
    /// Platform flag for the three macOS-shaped bits of the SYSTEM TRAY group:
    /// the "MENU BAR" header swap, the Close-to-tray description arm and the
    /// Hide-Dock row's `visible`. Compile-time, exactly like the Slint's
    /// `AppearanceState.is-macos`.
    #[serde(rename = "isMacos")]
    pub is_macos: bool,
    #[serde(rename = "isWindows")]
    pub is_windows: bool,
    /// A tray backend exists on this platform (`tray_qt::init` would create
    /// one). Phase A: Linux + macOS. Plan A-2 Task 5 adds Windows.
    #[serde(rename = "traySupported")]
    pub tray_supported: bool,
    /// `notify.rs` has a real arm here. Phase A: Linux + macOS. Plan A-2
    /// Task 3 adds Windows.
    #[serde(rename = "systemNotificationsSupported")]
    pub system_notifications_supported: bool,
    /// The RENDERER group offers a real choice: Linux (Vulkan/GL/software)
    /// and Windows (D3D11/D3D12/GL/software). macOS is always Metal.
    #[serde(rename = "rendererSelectable")]
    pub renderer_selectable: bool,
    #[serde(rename = "trayIconThemes")]
    pub tray_icon_themes: Vec<String>,
    #[serde(rename = "trayIconThemeIndex")]
    pub tray_icon_theme_index: i32,
    // Integrations (phase 19)
    #[serde(rename = "showRecommendations")]
    pub show_recommendations: bool,
    #[serde(rename = "musicbrainzEnabled")]
    pub musicbrainz_enabled: bool,
    #[serde(rename = "scrobbleEnabled")]
    pub scrobble_enabled: bool,
    #[serde(rename = "scrobbleUiCollapsed")]
    pub scrobble_ui_collapsed: bool,
    #[serde(rename = "allowLoggedOutScrobbling")]
    pub allow_logged_out_scrobbling: bool,
    /// Privacy: the local listen log (`listen_log.db`) is recording.
    #[serde(rename = "listenHistoryEnabled")]
    pub listen_history_enabled: bool,
    #[serde(rename = "lastfmEnabled")]
    pub lastfm_enabled: bool,
    #[serde(rename = "lastfmAuthed")]
    pub lastfm_authed: bool,
    #[serde(rename = "lastfmUsername")]
    pub lastfm_username: String,
    #[serde(rename = "lastfmAuthUrl")]
    pub lastfm_auth_url: String,
    #[serde(rename = "lastfmBusy")]
    pub lastfm_busy: bool,
    #[serde(rename = "listenbrainzEnabled")]
    pub listenbrainz_enabled: bool,
    #[serde(rename = "listenbrainzAuthed")]
    pub listenbrainz_authed: bool,
    #[serde(rename = "listenbrainzUsername")]
    pub listenbrainz_username: String,
    #[serde(rename = "listenbrainzBusy")]
    pub listenbrainz_busy: bool,
    #[serde(rename = "integrationsStatusText")]
    pub integrations_status_text: String,
    #[serde(rename = "integrationsStatusKind")]
    pub integrations_status_kind: i32,
    #[serde(rename = "discordEnabled")]
    pub discord_enabled: bool,
    /// Settings > Integrations > "Open Qobuz links in QBZ" (link_handler_qt).
    #[serde(rename = "qobuzLinksEnabled")]
    pub qobuz_links_enabled: bool,
    // --- Per-section sub-documents (settings_qt/*.rs) ---------------------
    /// Settings > Local Library (folders, scan, maintenance, Plex fields).
    pub library: library::Snapshot,
    /// Settings > Offline (offline MODE + the lyrics cache row).
    pub offline: offline::Snapshot,
    /// Settings > Developer + Blacklist counters + the sandbox gate.
    pub dev: devtools::Snapshot,
    /// Settings > Import / Export (settings bundle, portable blacklist,
    /// account migration).
    #[serde(rename = "importExport")]
    pub import_export: import_export::Snapshot,
    /// Settings > Network (outgoing proxy, QConnect LAN / Cast safety
    /// switches).
    pub network: network::Snapshot,
}

/// Index -> value maps the select handlers resolve against.
static MAPS: Mutex<(Vec<AudioBackendType>, Vec<String>)> = Mutex::new((Vec::new(), Vec::new()));

/// Last device enumeration (backend, rows, id map, taken at).
///
/// Every settings change rebuilds the whole document, and a library scan
/// re-publishes it on a 2 s ticker — enumerating the backend's devices that
/// often is wasted work (a PipeWire enumeration opens a connection). The
/// short TTL keeps hot-plug latency at a few seconds, and the refresh button
/// (which exists precisely for "my DAC is not listed") drops the entry.
static DEVICE_CACHE: Mutex<Option<(AudioBackendType, Vec<DeviceOption>, Vec<String>, Instant)>> =
    Mutex::new(None);

const DEVICE_CACHE_TTL: Duration = Duration::from_secs(4);

fn cached_devices(backend: AudioBackendType) -> (Vec<DeviceOption>, Vec<String>) {
    if let Ok(guard) = DEVICE_CACHE.lock() {
        if let Some((cached_backend, rows, ids, at)) = guard.as_ref() {
            if *cached_backend == backend && at.elapsed() < DEVICE_CACHE_TTL {
                return (rows.clone(), ids.clone());
            }
        }
    }
    let fresh = enumerate_devices(backend);
    if let Ok(mut guard) = DEVICE_CACHE.lock() {
        *guard = Some((backend, fresh.0.clone(), fresh.1.clone(), Instant::now()));
    }
    fresh
}

fn invalidate_device_cache() {
    if let Ok(mut guard) = DEVICE_CACHE.lock() {
        *guard = None;
    }
}

/// settings.rs `alsa_section` — Tauri dropdown sectioning for ALSA rows.
fn alsa_section(id: &str, is_default: bool, label: &str) -> usize {
    let id_l = id.to_ascii_lowercase();
    if id.is_empty() || id_l == "default" || is_default {
        0 // Defaults
    } else if id_l.starts_with("hw:")
        || id_l.starts_with("iec958:")
        || id_l.starts_with("front:card=")
        || label.to_ascii_lowercase().contains("bit-perfect")
    {
        1 // Bit-perfect (Hardware / Digital)
    } else if id_l.starts_with("plughw:") {
        2 // Plugin Hardware
    } else {
        3 // Other Outputs
    }
}

fn device_is_bit_perfect(backend: AudioBackendType, device: &qbz_audio::AudioDevice) -> bool {
    match backend {
        AudioBackendType::Alsa => {
            let label = device.description.as_deref().unwrap_or(&device.name);
            alsa_section(&device.id, device.is_default, label) == 1
        }
        AudioBackendType::PipeWire => device.is_hardware,
        _ => false,
    }
}

/// Enumerate output devices for a backend (settings.rs `enumerate_devices`
/// with the ALSA regrouping). Blocking — call off the async executor's
/// fast path (runs inside spawn_blocking by the caller).
fn enumerate_devices(backend: AudioBackendType) -> (Vec<DeviceOption>, Vec<String>) {
    let mut rows = vec![DeviceOption {
        label: qbz_i18n::t("System default"),
        bp: false,
        group: String::new(),
    }];
    let mut ids = vec![String::new()];
    match BackendManager::create_backend(backend).and_then(|b| b.enumerate_devices()) {
        Ok(devices) => {
            for d in devices {
                let label = match d.description.as_deref() {
                    Some(desc) if !desc.is_empty() => desc.to_string(),
                    _ => d.name.clone(),
                };
                ids.push(d.id.clone());
                rows.push(DeviceOption {
                    bp: device_is_bit_perfect(backend, &d),
                    label,
                    group: String::new(),
                });
            }
        }
        Err(e) => log::warn!("[qbz-qt] device enumeration failed: {e}"),
    }

    if backend == AudioBackendType::Alsa {
        // Stable sort by section; the section header lands on each section's
        // first row (settings.rs `group_alsa_devices`). rows[i] aligns with
        // ids[i] (both lead with the synthetic "System default"/"" entry).
        let section_labels = [
            qbz_i18n::t("Defaults"),
            qbz_i18n::t("Bit-perfect (Hardware / Digital)"),
            qbz_i18n::t("Plugin Hardware"),
            qbz_i18n::t("Other Outputs"),
        ];
        let mut indexed: Vec<(usize, DeviceOption, String)> = rows
            .into_iter()
            .zip(ids.iter().cloned())
            .enumerate()
            .map(|(i, (row, id))| (alsa_section(&id, i == 0, &row.label), row, id))
            .collect();
        indexed.sort_by_key(|(section, _, _)| *section);
        // Rebuild ids in the SAME order (they're the index map).
        let mut out_rows = Vec::with_capacity(indexed.len());
        let mut out_ids = Vec::with_capacity(indexed.len());
        let mut prev: Option<usize> = None;
        for (section, mut row, id) in indexed {
            if prev != Some(section) {
                prev = Some(section);
                row.group = section_labels[section].clone();
            }
            out_rows.push(row);
            out_ids.push(id);
        }
        (out_rows, out_ids)
    } else {
        (rows, ids)
    }
}

/// The ACTIVE backend's display label, for anything outside this module that
/// needs to name it in prose (the log viewer's diagnostics bundle header).
pub fn current_backend_label() -> String {
    backend_label(audio_settings().backend_type.unwrap_or_default())
}

fn backend_label(t: AudioBackendType) -> String {
    match t {
        AudioBackendType::PipeWire => "PipeWire".to_string(),
        AudioBackendType::Alsa => "ALSA".to_string(),
        AudioBackendType::Pulse => "PulseAudio".to_string(),
        AudioBackendType::SystemDefault => qbz_i18n::t("System default"),
        AudioBackendType::Jack => "JACK".to_string(),
        // Not translated, like the other backend proper nouns above it.
        AudioBackendType::WasapiExclusive => "WASAPI Exclusive".to_string(),
    }
}

/// Build + publish the full snapshot (settings.rs `build_snapshot`).
pub async fn publish_snapshot() {
    let audio_settings = audio_settings();
    let prefs = with_playback(|s| s.get_preferences()).unwrap_or_default();
    // Re-seed the persistence gates from what we just read. `init_for_user`
    // seeds them at login and the two toggles push them live, so this only
    // matters when the prefs changed OUTSIDE this process — the Slint build
    // writes the same per-user DB. Cheap, and it makes the gates self-healing
    // instead of trusting that nothing else ever touches the file.
    qbz_app::session_persist::set_gates(prefs.persist_session, prefs.resume_playback_position);
    let streaming_key = streaming_quality();

    // #638 fix 3 — read the cap cache, never probe here. `publish_snapshot`
    // runs on every settings mutation and on every Settings open; the probe is
    // a `pw-dump` subprocess and belongs only on the six explicit triggers
    // (`refresh_device_cap`). This is two uncontended lock reads.
    let (device_cap_summary, device_cap_detected) = qbz_app::device_cap::summary();

    let doc = tokio::task::spawn_blocking(move || {
        let backend_types = BackendManager::available_backends();
        let current_backend = audio_settings.backend_type.unwrap_or_default();
        let backend_index = backend_types
            .iter()
            .position(|t| *t == current_backend)
            .unwrap_or(0);
        let active_backend = backend_types
            .get(backend_index)
            .copied()
            .unwrap_or_default();

        let (devices, ids) = cached_devices(active_backend);
        let device_index = match &audio_settings.output_device {
            None => 0,
            Some(id) => ids.iter().position(|d| d == id).unwrap_or(0),
        };
        let device_label = devices
            .get(device_index)
            .map(|device| device.label.clone())
            .unwrap_or_else(|| qbz_i18n::t("System default"));

        // Settings owns hardware enumeration, so this is the one edge that
        // can publish the NPB tooltip with a human-readable device name.
        // Track/stream edges reuse the cached label and never probe hardware.
        crate::output_labels::publish_with_device_label(&audio_settings, device_label);

        let alsa_plugin = audio_settings.alsa_plugin.unwrap_or(AlsaPlugin::Hw);
        let alsa_plugin_index = ALSA_PLUGIN_VALUES
            .iter()
            .position(|p| *p == alsa_plugin)
            .unwrap_or(0);
        let retry_behavior_index = RETRY_BEHAVIOR_VALUES
            .iter()
            .position(|v| *v == audio_settings.quality_fallback_behavior)
            .unwrap_or(0);
        let qconnect_startup = crate::qconnect_transport_qt::load_startup_mode()
            .as_str()
            .to_string();
        let qconnect_conflict_policy =
            crate::qconnect_transport_qt::load_playback_conflict_policy();
        let scrobble_snap = crate::integrations_qt::scrobble_settings();
        let integ_ui = crate::integrations_qt::ui_snapshot();
        let qconnect_startup_index = QCONNECT_STARTUP_VALUES
            .iter()
            .position(|v| *v == qconnect_startup)
            .unwrap_or(QCONNECT_STARTUP_VALUES.len() - 1);
        let streaming_index = STREAMING_QUALITY_KEYS
            .iter()
            .position(|k| *k == streaming_key)
            .unwrap_or(STREAMING_QUALITY_KEYS.len() - 1);
        let hardware_volume_ui = hardware_volume_ui_snapshot(&audio_settings);

        let mut maps = MAPS.lock().unwrap();
        maps.0 = backend_types.clone();
        maps.1 = ids;

        SettingsDoc {
            streaming_qualities: STREAMING_QUALITY_LABELS
                .iter()
                .map(|l| qbz_i18n::t(l))
                .collect(),
            streaming_quality_index: streaming_index as i32,
            backends: std::iter::once(qbz_i18n::t("Auto"))
                .chain(backend_types.iter().map(|t| backend_label(*t)))
                .collect(),
            backend_index: backend_index as i32 + 1,
            backend_is_alsa: active_backend == AudioBackendType::Alsa,
            backend_is_wasapi: active_backend == AudioBackendType::WasapiExclusive,
            backend_is_coreaudio: cfg!(target_os = "macos")
                && active_backend == AudioBackendType::SystemDefault,
            backend_is_pipewire: active_backend == AudioBackendType::PipeWire,
            backend_is_jack: active_backend == AudioBackendType::Jack,
            devices,
            device_index: device_index as i32,
            alsa_plugins: ALSA_PLUGIN_LABELS.iter().map(|l| qbz_i18n::t(l)).collect(),
            alsa_plugin_index: alsa_plugin_index as i32,
            alsa_plugin_is_hw: alsa_plugin == AlsaPlugin::Hw,
            alsa_direct_selected: qbz_audio::alsa_direct::uses_alsa_direct_route(&audio_settings),
            alsa_hardware_volume: audio_settings.alsa_hardware_volume,
            alsa_hardware_volume_options: hardware_volume_ui.options,
            alsa_hardware_volume_index: hardware_volume_ui.selected_index,
            alsa_hardware_volume_selected: hardware_volume_ui.selected,
            alsa_hardware_volume_status: hardware_volume_ui.status,
            alsa_hardware_volume_message: hardware_volume_ui.message,
            alsa_hardware_volume_can_change: hardware_volume_ui.can_change,
            alsa_hardware_volume_choice_token: hardware_volume_ui.choice_token,
            dsd_modes: DSD_MODE_LABELS.iter().map(|l| qbz_i18n::t(l)).collect(),
            dsd_mode_index: DSD_MODE_VALUES
                .iter()
                .position(|v| *v == audio_settings.dsd_mode)
                .unwrap_or(0) as i32,
            limit_quality_to_device: audio_settings.limit_quality_to_device,
            device_cap_summary,
            device_cap_detected,
            exclusive_mode: audio_settings.exclusive_mode,
            reserve_dac: audio_settings.reserve_dac_while_running,
            dac_passthrough: audio_settings.dac_passthrough,
            pw_force_bitperfect: audio_settings.pw_force_bitperfect,
            allow_quality_fallback: audio_settings.allow_quality_fallback,
            sync_audio_on_startup: audio_settings.sync_audio_on_startup,
            skip_sink_switch: audio_settings.skip_sink_switch,
            continue_playback: prefs.autoplay_mode == AutoplayMode::ContinueWithinSource,
            show_context_icon: prefs.show_context_icon,
            gapless: audio_settings.gapless_enabled,
            normalization: audio_settings.normalization_enabled,
            normalization_targets: {
                let mut v: Vec<String> = NORMALIZATION_TARGETS
                    .iter()
                    .map(|(l, _)| qbz_i18n::t(l))
                    .collect();
                if normalization_target_index(audio_settings.normalization_target_lufs).is_none() {
                    v.push(qbz_i18n::t_args(
                        "Custom ({} LUFS)",
                        &[&format!("{:.0}", audio_settings.normalization_target_lufs)],
                    ));
                }
                v
            },
            normalization_target_index: normalization_target_index(
                audio_settings.normalization_target_lufs,
            )
            .unwrap_or(NORMALIZATION_TARGETS.len()) as i32,
            normalization_prevent_clipping: audio_settings.normalization_prevent_clipping,
            persist_session: prefs.persist_session,
            resume_position: prefs.resume_playback_position,
            stream_uncached: audio_settings.stream_first_track,
            buffer_seconds: audio_settings.stream_buffer_seconds as i32,
            streaming_only: audio_settings.streaming_only,
            playback_cache: audio_settings.playback_cache.clone(),
            playback_memory_profile: audio_settings.playback_cache.selected_profile().as_str().into(),
            appearance_profile: appearance_profile::selected(),
            playback_memory_apply_busy: crate::playback_qt::memory_apply_busy(),
            playback_storage: playback_storage::snapshot(),
            playback_cache_usage: crate::APP.get().map(|r| r.core().player().playback_cache_stats()),
            retry_behaviors: RETRY_BEHAVIOR_LABELS
                .iter()
                .map(|l| qbz_i18n::t(l))
                .collect(),
            retry_behavior_index: retry_behavior_index as i32,
            qconnect_startup_modes: QCONNECT_STARTUP_LABELS
                .iter()
                .map(|l| qbz_i18n::t(l))
                .collect(),
            qconnect_startup_index: qconnect_startup_index as i32,
            qconnect_conflict_policies: QCONNECT_CONFLICT_POLICY_LABELS
                .iter()
                .map(|label| qbz_i18n::t(label))
                .collect(),
            qconnect_conflict_policy_index: qconnect_conflict_policy.index(),
            qconnect_device_name: crate::qconnect_transport_qt::load_persisted_device_name()
                .unwrap_or_default(),
            qconnect_device_name_default:
                crate::qconnect_transport_qt::resolve_qconnect_friendly_name(None),
            album_header_gradient: pref_bool("album_header_gradient", true),
            compact_album_header: pref_bool("compact_album_header", false),
            app_background_modes: APP_BACKGROUND_LABELS
                .iter()
                .map(|l| qbz_i18n::t(l))
                .collect(),
            app_background_index: APP_BACKGROUND_VALUES
                .iter()
                .position(|v| *v == pref_str("app_background", "off"))
                .unwrap_or(0) as i32,
            app_background_image: pref_str("app_background_image", ""),
            theme_custom_collapsed: pref_bool("theme_custom_collapsed", false),
            theme_auto_collapsed: pref_bool("theme_auto_collapsed", false),
            ambient_options_collapsed: pref_bool("ambient_options_collapsed", false),
            auto_theme_sources: AUTO_THEME_SOURCE_LABELS
                .iter()
                .map(|l| qbz_i18n::t(l))
                .collect(),
            auto_theme_source_index: index_of(
                AUTO_THEME_SOURCE_VALUES,
                &pref_str("auto_theme_source", "system"),
                0,
            ),
            auto_theme_image_path: pref_str("auto_theme_image_path", ""),
            // The SHARED detector (`qbz_theme::auto::detect_desktop_environment`),
            // the same one the generator itself consults — a second guess here
            // could name a desktop the palette did not come from.
            auto_theme_detected_de: qbz_theme::auto::detect_desktop_environment()
                .display_name()
                .to_string(),
            intelligent_search: pref_bool("intelligent_search", true),
            languages: LANGUAGE_LABELS.iter().map(|l| qbz_i18n::t(l)).collect(),
            language_index: index_of(LANGUAGE_VALUES, &pref_str("language", "auto"), 0),
            ui_scales: UI_SCALE_LABELS.iter().map(|l| qbz_i18n::t(l)).collect(),
            ui_scale_index: index_of(UI_SCALE_VALUES, &pref_str("ui_scale", "default"), 2),
            // Only the first label is a word; the rest are typeface names and
            // are passed through untranslated.
            app_fonts: APP_FONT_LABELS
                .iter()
                .enumerate()
                .map(|(i, l)| {
                    if i == 0 {
                        qbz_i18n::t(l)
                    } else {
                        (*l).to_string()
                    }
                })
                .collect(),
            app_font_index: app_font_index(),
            immersive_search_actions: IMMERSIVE_SEARCH_LABELS
                .iter()
                .map(|l| qbz_i18n::t(l))
                .collect(),
            immersive_search_action_index: index_of(
                IMMERSIVE_SEARCH_VALUES,
                &pref_str("immersive_search_action", "replace"),
                1,
            ),
            immersive_default_views: IMMERSIVE_VIEW_LABELS
                .iter()
                .map(|l| qbz_i18n::t(l))
                .collect(),
            immersive_default_view_index: index_of(
                IMMERSIVE_VIEW_VALUES,
                &pref_str("immersive_default_view", "remember"),
                0,
            ),
            nav_in_sidebar: nav_in_sidebar(),
            nav_header_compact: nav_header_compact(),
            collapsible_myqbz: collapsible_myqbz(),
            myqbz_show_mixtapes: myqbz_show_mixtapes(),
            myqbz_show_collections: myqbz_show_collections(),
            hide_standard_myqbz: hide_standard_myqbz(),
            sidebar_playlists_collapsed: sidebar_playlists_collapsed(),
            myqbz_collapsed: myqbz_collapsed(),
            myqbz_mixtapes_collapsed: myqbz_mixtapes_collapsed(),
            myqbz_collections_collapsed: myqbz_collections_collapsed(),
            my_qbz_label: myqbz_label(),
            sidebar_playlist_collage: pref_bool("sidebar_playlist_collage", true),
            queue_track_artwork: pref_bool("queue_track_artwork", false),
            library_track_artwork: pref_bool("library_track_artwork", false),
            local_library_track_artwork: pref_bool("local_library_track_artwork", false),
            play_indicator_animation: pref_bool("play_indicator_animation", false),
            show_featured_artists: pref_bool("show_featured_artists", true),
            seekbar_waveform: seekbar_waveform(),
            invert_swipe_navigation: pref_bool("invert_swipe_navigation", false),
            in_app_toasts: pref_bool("in_app_toasts", true),
            system_notifications: pref_bool("system_notifications", true),
            window_title_show: pref_bool("window_title_show", false),
            use_system_title_bar: use_system_title_bar(),
            hide_title_bar: pref_bool("hide_title_bar", false),
            wc_positions: WC_POSITION_LABELS.iter().map(|l| qbz_i18n::t(l)).collect(),
            wc_position_index: index_of(WC_POSITION_VALUES, &pref_str("wc_position", "right"), 1),
            show_window_controls: pref_bool("show_window_controls", true),
            show_volume_steppers: pref_bool("show_volume_steppers", false),
            show_skip_ten: pref_bool("show_skip_ten", false),
            mini_default_views: MINI_VIEW_LABELS.iter().map(|l| qbz_i18n::t(l)).collect(),
            mini_default_view_index: index_of(
                MINI_VIEW_VALUES,
                &pref_str("mini_default_view", "remember"),
                0,
            ),
            startup_pages: STARTUP_PAGE_LABELS.iter().map(|l| qbz_i18n::t(l)).collect(),
            startup_page_index: index_of(STARTUP_PAGE_VALUES, &pref_str("startup_page", "home"), 0),
            show_purchases: pref_bool("show_purchases", false),
            nav_click_first_tab: pref_bool("nav_click_first_tab", false),
            local_tab_order: local_tab_order(),
            genre_filters_position: local_genre_filters_position(),
            renderers: RENDERER_LABELS.iter().map(|l| qbz_i18n::t(l)).collect(),
            renderer_index: index_of(RENDERER_VALUES, &pref_str("renderer", "auto"), 0),
            gpu_powers: gpu_power_choice().0,
            gpu_power_index: gpu_power_choice().1,
            gpu_selectable: crate::renderer_qt::gpu_selectable(),
            tray_enable: tray().get_settings().map(|t| t.enable_tray).unwrap_or(true),
            // S1: the shared default is `close_to_tray: true`
            // (`qbz-app/src/settings/tray.rs:61`), and the Slint glue reads it
            // with `unwrap_or_default()` on the struct
            // (`crates/qbz/src/tray_settings.rs:54-56`). This line used to fall
            // back to `false`, which was cosmetic while nothing consumed it and
            // becomes BEHAVIOURAL with the tray: with no store bound, the
            // user's very first close would quit instead of hiding. The
            // adjacent `tray_enable` read above already uses `.unwrap_or(true)`
            // against `tray.rs:59` — the disagreement between two sibling reads
            // of one store was the smell.
            tray_close_to_tray: tray()
                .get_settings()
                .map(|t| t.close_to_tray)
                .unwrap_or(true),
            // Both shared defaults are `false` (`qbz-app/src/settings/tray.rs:59-63`),
            // so unlike the two reads above these fall back to `false` — the
            // fallback must match the struct's own default, which is the whole
            // point of the S1 fix directly above.
            tray_minimize_to_tray: tray()
                .get_settings()
                .map(|t| t.minimize_to_tray)
                .unwrap_or(false),
            tray_mac_hide_dock: tray()
                .get_settings()
                .map(|t| t.mac_hide_dock)
                .unwrap_or(false),
            is_macos: cfg!(target_os = "macos"),
            is_windows: cfg!(target_os = "windows"),
            // Windows joined with A-2 T5 (Shell_NotifyIconW).
            tray_supported: cfg!(any(
                target_os = "linux",
                target_os = "macos",
                target_os = "windows"
            )),
            // Windows answers at RUNTIME, not by cfg: the toast arm is
            // implemented, but delivery needs a registered AppUserModelID,
            // which only the MSI provides. A portable unzip gets `false` and
            // the row stays hidden rather than offering a switch that can
            // never produce a notification.
            system_notifications_supported: {
                #[cfg(target_os = "windows")]
                {
                    windows_toast_identity_registered()
                }
                #[cfg(not(target_os = "windows"))]
                {
                    cfg!(any(target_os = "linux", target_os = "macos"))
                }
            },
            renderer_selectable: cfg!(any(target_os = "linux", target_os = "windows")),
            tray_icon_themes: TRAY_ICON_LABELS.iter().map(|l| qbz_i18n::t(l)).collect(),
            tray_icon_theme_index: tray()
                .get_settings()
                .map(|t| index_of(TRAY_ICON_VALUES, &t.tray_icon_theme, 0))
                .unwrap_or(0),
            show_recommendations: crate::integrations_qt::show_recommendations(),
            musicbrainz_enabled: pref_bool("musicbrainz_enabled", true),
            scrobble_enabled: scrobble_snap.enabled,
            scrobble_ui_collapsed: scrobble_snap.ui_collapsed,
            allow_logged_out_scrobbling: scrobble_snap.allow_logged_out_scrobbling,
            listen_history_enabled: crate::listen_log_qt::is_enabled(),
            lastfm_enabled: scrobble_snap.lastfm_enabled,
            lastfm_authed: scrobble_snap.lastfm_is_authed(),
            lastfm_username: scrobble_snap.lastfm_username.clone(),
            lastfm_auth_url: integ_ui.lastfm_auth_url.clone(),
            lastfm_busy: integ_ui.lastfm_busy,
            listenbrainz_enabled: scrobble_snap.listenbrainz_enabled,
            listenbrainz_authed: scrobble_snap.listenbrainz_is_authed(),
            listenbrainz_username: scrobble_snap.listenbrainz_username.clone(),
            listenbrainz_busy: integ_ui.listenbrainz_busy,
            integrations_status_text: integ_ui.status_text.clone(),
            integrations_status_kind: integ_ui.status_kind,
            discord_enabled: crate::integrations_qt::discord_enabled(),
            qobuz_links_enabled: crate::link_handler_qt::is_enabled(),
            library: library::snapshot(),
            offline: offline::snapshot(),
            dev: devtools::snapshot(),
            import_export: import_export::snapshot(),
            network: network::snapshot(),
        }
    })
    .await
    .unwrap_or_default();

    let json = serde_json::to_string(&doc).unwrap_or_else(|_| "{}".into());
    crate::ui(move |mut b| {
        b.as_mut().set_settings_json(QString::from(json.as_str()));
    });
}

// ---------------------------------------------------------------------------
// Apply (settings.rs `apply_audio`) — the ONLY player touchpoints.
// ---------------------------------------------------------------------------

/// What a change requires of the live player (settings.rs `Apply`).
#[derive(Clone, Copy)]
enum Apply {
    None,
    Reload,
    Reinit,
}

/// Admit one live audio mutation owned by the local settings surface. Some
/// callers persist before they reach the live-player seam, so the refusal is
/// explicit about that split instead of pretending to roll back half an audio
/// settings transaction.
fn begin_audio_owner_action(
    action: &str,
    preference_may_be_saved: bool,
) -> Option<crate::playback_qt::OwnerActionLease> {
    match crate::playback_qt::begin_owner_action() {
        Some(permit) => Some(permit),
        None => {
            log::info!("[qbz-qt] audio settings: {action} refused while QConnect owns playback");
            let message = if preference_may_be_saved {
                qbz_i18n::t(
                    "The audio setting was saved but not applied to live playback because QConnect is in control. Try again after playback returns to QBZ.",
                )
            } else {
                qbz_i18n::t(
                    "QConnect is controlling playback. Return playback to QBZ and try again.",
                )
            };
            crate::toast_qt::info(message);
            None
        }
    }
}

async fn probe_selected_hardware_volume(
    audio: &qbz_audio::settings::AudioSettings,
) -> Result<(HardwareVolumeProbe, HardwareVolumeDecision), String> {
    if !qbz_audio::alsa_direct::uses_alsa_direct_route(audio) {
        return Err(
            "hardware volume requires an explicitly selected ALSA Direct device".to_string(),
        );
    }
    let device_id = audio
        .output_device
        .clone()
        .ok_or_else(|| "ALSA Direct device is missing".to_string())?;
    let persisted = audio.alsa_hardware_volume_controls.clone();
    tokio::task::spawn_blocking(move || {
        let probe = qbz_audio::alsa_hardware_volume::enumerate_hardware_volume_controls(&device_id);
        let decision = qbz_audio::alsa_hardware_volume::decide_hardware_volume(&probe, &persisted);
        (probe, decision)
    })
    .await
    .map_err(|error| format!("ALSA hardware-volume probe task failed: {error}"))
}

fn seed_hardware_volume(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    control: &AlsaMixerControlId,
    volume: f32,
) {
    // Update SharedState without touching the old output. The new direct
    // engine reads this after Reinit and writes back the same level the probe
    // sampled, instead of copying the locked path's synthetic 100% into the
    // physical mixer (or writing the new device's level through the old one).
    runtime.core().player().seed_volume_state(volume);
    save_pref("volume", serde_json::json!(volume));
    crate::now_playing::set_volume(volume);
    log::info!(
        "[qbz-qt] ALSA hardware volume: '{}' available at {:.0}%",
        control,
        volume * 100.0
    );
}

fn set_hardware_volume_ui_status(device_id: Option<String>, status: HardwareVolumeUiStatus) {
    if let Ok(mut state) = HARDWARE_VOLUME_UI.lock() {
        if state.device_id != device_id {
            *state = HardwareVolumeUiState {
                device_id,
                status,
                ..Default::default()
            };
        } else {
            state.status = status;
        }
    }
}

fn set_hardware_volume_ui_probe(
    device_id: String,
    probe: &HardwareVolumeProbe,
    selected: Option<AlsaMixerControlId>,
    status: HardwareVolumeUiStatus,
) {
    if let Ok(mut state) = HARDWARE_VOLUME_UI.lock() {
        *state = HardwareVolumeUiState {
            device_id: Some(device_id),
            route_key: probe.route_key.clone(),
            candidates: probe.valid_candidates().cloned().collect(),
            selected,
            status,
        };
    }
}

fn hardware_volume_unsupported_status(
    reason: &HardwareVolumeUnsupportedReason,
) -> HardwareVolumeUiStatus {
    match reason {
        HardwareVolumeUnsupportedReason::Probe(error) => match error.kind {
            HardwareVolumeProbeErrorKind::PermissionDenied => {
                HardwareVolumeUiStatus::PermissionDenied
            }
            HardwareVolumeProbeErrorKind::DeviceBusy => HardwareVolumeUiStatus::DeviceBusy,
            HardwareVolumeProbeErrorKind::DeviceUnavailable => {
                HardwareVolumeUiStatus::DeviceUnavailable
            }
            HardwareVolumeProbeErrorKind::NoMixer => HardwareVolumeUiStatus::NoMixer,
            HardwareVolumeProbeErrorKind::InvalidDevice
            | HardwareVolumeProbeErrorKind::UnsupportedPlatform
            | HardwareVolumeProbeErrorKind::Other => HardwareVolumeUiStatus::Unsupported,
        },
        // No valid alternative exists in this decision variant. Reserve the
        // actionable Stale state for NeedsChoice, where a closed selector can
        // actually offer another enumerated control.
        HardwareVolumeUnsupportedReason::PersistedSelectionStale(_) => {
            HardwareVolumeUiStatus::Unsupported
        }
        HardwareVolumeUnsupportedReason::NoValidControls => HardwareVolumeUiStatus::Unsupported,
    }
}

fn hardware_volume_choice_status(reason: HardwareVolumeChoiceReason) -> HardwareVolumeUiStatus {
    match reason {
        HardwareVolumeChoiceReason::PersistedSelectionStale => HardwareVolumeUiStatus::Stale,
        HardwareVolumeChoiceReason::Ambiguous | HardwareVolumeChoiceReason::UcmAmbiguous => {
            HardwareVolumeUiStatus::Ambiguous
        }
    }
}

fn current_hardware_volume_route_is(
    expected_device_id: &str,
) -> Result<qbz_audio::settings::AudioSettings, String> {
    let current = with_audio(|store| store.get_settings())?;
    if !qbz_audio::alsa_direct::uses_alsa_direct_route(&current)
        || current.output_device.as_deref() != Some(expected_device_id)
    {
        return Err(
            "the selected ALSA route changed while its mixer was being checked".to_string(),
        );
    }
    Ok(current)
}

async fn activate_hardware_volume_selection(
    device_id: &str,
    control: &AlsaMixerControlId,
) -> Result<HardwareVolumeSnapshot, String> {
    let device_id = device_id.to_string();
    let control = control.clone();
    tokio::task::spawn_blocking(move || {
        qbz_audio::alsa_hardware_volume::activate_hardware_volume_control(&device_id, &control)
    })
    .await
    .map_err(|error| format!("ALSA hardware-volume validation task failed: {error}"))?
}

async fn enable_hardware_volume_selection(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    device_id: String,
    probe: HardwareVolumeProbe,
    selection: HardwareVolumeSelection,
) -> Result<Apply, String> {
    current_hardware_volume_route_is(&device_id)?;
    let snapshot = match activate_hardware_volume_selection(&device_id, &selection.control).await {
        Ok(snapshot) => snapshot,
        Err(error) => {
            log::warn!(
                "[qbz-qt] ALSA hardware-volume control '{}' failed enable validation: {error}",
                selection.control
            );
            with_audio(|store| store.set_alsa_hardware_volume(false))?;
            set_hardware_volume_ui_probe(
                device_id,
                &probe,
                Some(selection.control),
                HardwareVolumeUiStatus::WriteFailed,
            );
            return Ok(Apply::Reinit);
        }
    };
    current_hardware_volume_route_is(&device_id)?;
    with_audio(|store| {
        store.enable_alsa_hardware_volume(selection.route_key, selection.control.clone())
    })?;
    set_hardware_volume_ui_probe(
        device_id,
        &probe,
        Some(selection.control.clone()),
        HardwareVolumeUiStatus::Selected,
    );
    seed_hardware_volume(runtime, &selection.control, snapshot.volume);
    Ok(Apply::Reinit)
}

async fn set_alsa_hardware_volume(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    enabled: bool,
) -> Result<Apply, String> {
    if !enabled {
        let audio = with_audio(|store| {
            store.set_alsa_hardware_volume(false)?;
            store.get_settings()
        })?;
        set_hardware_volume_ui_status(audio.output_device, HardwareVolumeUiStatus::Disabled);
        return Ok(Apply::Reinit);
    }

    let audio = with_audio(|store| store.get_settings())?;
    let device_id = audio
        .output_device
        .clone()
        .ok_or_else(|| "ALSA Direct device is missing".to_string())?;
    set_hardware_volume_ui_status(Some(device_id.clone()), HardwareVolumeUiStatus::Probing);
    publish_snapshot().await;

    let (probe, decision) = match probe_selected_hardware_volume(&audio).await {
        Ok(result) => result,
        Err(error) => {
            let _ = with_audio(|store| store.set_alsa_hardware_volume(false));
            set_hardware_volume_ui_status(Some(device_id), HardwareVolumeUiStatus::Unsupported);
            log::warn!("[qbz-qt] ALSA hardware-volume probe failed: {error}");
            return Ok(Apply::Reinit);
        }
    };
    current_hardware_volume_route_is(&device_id)?;
    match decision {
        HardwareVolumeDecision::Selected { selection } => {
            enable_hardware_volume_selection(runtime, device_id, probe, selection).await
        }
        HardwareVolumeDecision::NeedsChoice { reason, .. } => {
            with_audio(|store| store.set_alsa_hardware_volume(false))?;
            let status = hardware_volume_choice_status(reason);
            set_hardware_volume_ui_probe(device_id, &probe, None, status);
            log::info!(
                "[qbz-qt] ALSA hardware volume needs an explicit control choice ({reason:?}); no mixer write performed"
            );
            Ok(Apply::Reinit)
        }
        HardwareVolumeDecision::Unsupported { reason, .. } => {
            with_audio(|store| store.set_alsa_hardware_volume(false))?;
            let status = hardware_volume_unsupported_status(&reason);
            set_hardware_volume_ui_probe(device_id, &probe, None, status);
            log::warn!("[qbz-qt] ALSA hardware volume is unavailable: {reason:?}");
            Ok(Apply::Reinit)
        }
    }
}

fn apply_audio_with_owner(runtime: &Arc<AppRuntime<LoggingAdapter>>, apply: Apply) {
    let reinit = match apply {
        Apply::None => return,
        Apply::Reload => false,
        Apply::Reinit => true,
    };
    let fresh = match with_audio(|s| s.get_settings()) {
        Ok(s) => s,
        Err(e) => {
            log::error!("[qbz-qt] re-read audio settings failed: {e}");
            return;
        }
    };
    let player = runtime.core().player();
    if let Err(e) = player.reload_settings(fresh.clone()) {
        log::error!("[qbz-qt] player.reload_settings failed: {e}");
    }
    if reinit {
        if let Err(e) = player.reinit_device(fresh.output_device.clone()) {
            log::error!("[qbz-qt] player.reinit_device failed: {e}");
        }
    }
    log::info!("[qbz-qt] audio settings applied to player (reinit={reinit})");
    if let Some(service) = crate::qconnect_qt::service() {
        crate::spawn(async move {
            if let Err(error) = service.report_device_info().await {
                log::warn!("[QConnect] output capabilities update failed: {error}");
            }
        });
    }
    // Republish the document. Without this a change made from the now-playing
    // bars' audio flyout persisted and took effect but NEVER reached the QML,
    // so the flyout's own switch snapped back to the stale value the next time
    // it opened — which is why both bars had grown local shadow state instead.
    // Cheap: one serialize + one Qt hop, and only on an actual audio apply.
    crate::publish_settings();
}

fn apply_audio(runtime: &Arc<AppRuntime<LoggingAdapter>>, apply: Apply) {
    if matches!(apply, Apply::None) {
        return;
    }
    let Some(_owner_action) = begin_audio_owner_action("apply live audio settings", true) else {
        return;
    };
    apply_audio_with_owner(runtime, apply);
}

/// Release the live output and wait for the audio thread to confirm that the
/// PCM/reservation is gone and any PipeWire sink QBZ suspended is awake.
/// `Player::release_device` is intentionally blocking because the ack is the
/// ordering guarantee; keep that wait off Tokio's worker threads.
async fn release_output_device_with_owner(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
) -> Result<(), String> {
    let player = runtime.core().player();
    tokio::task::spawn_blocking(move || player.release_device())
        .await
        .map_err(|error| format!("audio-device release task failed: {error}"))?
}

fn report_release_failure(error: &str) {
    log::error!("[qbz-qt] output-device release failed: {error}");
    crate::toast_qt::error(qbz_i18n::t(
        "QBZ could not fully release the previous audio device. See the audio log for details.",
    ));
}

fn requires_alsa_direct_unity(audio: &qbz_audio::settings::AudioSettings) -> bool {
    qbz_audio::alsa_direct::uses_alsa_direct_route(audio) && !audio.alsa_hardware_volume
}

/// Keep software gain at unity for ALSA-direct output. The public core
/// volume seam does not reconfigure the protected device/backend; the DAC owns
/// level in this mode. A QConnect peer is deliberately exempt because its
/// remote volume is the active control.
async fn force_bitperfect_volume_with_owner(runtime: &Arc<AppRuntime<LoggingAdapter>>) {
    let audio = match with_audio(|store| store.get_settings()) {
        Ok(audio) => audio,
        Err(error) => {
            log::error!("[qbz-qt] re-read audio for force-100 failed: {error}");
            return;
        }
    };
    if !requires_alsa_direct_unity(&audio) {
        return;
    }
    let controlling_peer = match crate::qconnect_qt::service() {
        Some(service) => service.is_peer_active().await,
        None => false,
    };
    if controlling_peer {
        return;
    }
    if let Err(error) = runtime.core().set_volume(1.0) {
        log::error!("[qbz-qt] force bit-perfect volume to 100 failed: {error}");
        return;
    }
    log::info!("[qbz-qt] bit-perfect: forced local volume to 100%");
    crate::now_playing::set_volume(1.0);
}

pub(crate) async fn maybe_force_bitperfect_volume(runtime: &Arc<AppRuntime<LoggingAdapter>>) {
    // Do not reject a delegated session merely for reading a setting whose
    // active route needs no live correction.
    if !requires_alsa_direct_unity(&audio_settings()) {
        return;
    }
    let Some(_owner_action) = begin_audio_owner_action("force bit-perfect volume", true) else {
        return;
    };
    force_bitperfect_volume_with_owner(runtime).await;
}

/// Revalidate a persisted hardware-volume preference after startup or a route
/// change. A successful probe synchronizes QBZ to the physical mixer before
/// any stream reinitialization. A failed probe disables the preference and
/// reloads the player, leaving the protected direct path locked at unity.
async fn reconcile_alsa_hardware_volume_with_owner(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
) -> Option<f32> {
    let audio = match with_audio(|store| store.get_settings()) {
        Ok(audio) => audio,
        Err(error) => {
            log::error!("[qbz-qt] re-read audio for hardware-volume probe failed: {error}");
            return None;
        }
    };
    if !qbz_audio::alsa_direct::uses_alsa_direct_route(&audio) {
        if audio.alsa_hardware_volume {
            if let Err(error) = with_audio(|store| store.set_alsa_hardware_volume(false)) {
                log::error!("[qbz-qt] failed to disable hardware volume off ALSA Direct: {error}");
            }
        }
        set_hardware_volume_ui_status(None, HardwareVolumeUiStatus::Disabled);
        return None;
    }

    let device_id = audio
        .output_device
        .clone()
        .expect("direct route has a device");
    if !audio.alsa_hardware_volume {
        let has_saved_control = qbz_audio::alsa_hardware_volume::stable_alsa_route_key(&device_id)
            .ok()
            .is_some_and(|route| audio.alsa_hardware_volume_controls.contains_key(&route));
        if !has_saved_control {
            set_hardware_volume_ui_status(Some(device_id), HardwareVolumeUiStatus::Disabled);
            return None;
        }
    }

    set_hardware_volume_ui_status(Some(device_id.clone()), HardwareVolumeUiStatus::Probing);
    let (probe, decision) = match probe_selected_hardware_volume(&audio).await {
        Ok(result) => result,
        Err(error) => {
            log::warn!("[qbz-qt] ALSA hardware-volume reconciliation failed: {error}");
            if audio.alsa_hardware_volume {
                let _ = with_audio(|store| store.set_alsa_hardware_volume(false));
            }
            set_hardware_volume_ui_status(Some(device_id), HardwareVolumeUiStatus::Unsupported);
            if audio.alsa_hardware_volume {
                apply_audio_with_owner(runtime, Apply::Reinit);
            }
            return None;
        }
    };

    if !audio.alsa_hardware_volume {
        match decision {
            HardwareVolumeDecision::Selected { selection }
                if selection.source == HardwareVolumeSelectionSource::Persisted =>
            {
                set_hardware_volume_ui_probe(
                    device_id,
                    &probe,
                    Some(selection.control),
                    HardwareVolumeUiStatus::Disabled,
                );
            }
            HardwareVolumeDecision::NeedsChoice { reason, .. } => {
                log::warn!(
                    "[qbz-qt] saved ALSA hardware-volume control needs a new choice ({reason:?})"
                );
                set_hardware_volume_ui_probe(
                    device_id,
                    &probe,
                    None,
                    HardwareVolumeUiStatus::Disabled,
                );
            }
            HardwareVolumeDecision::Unsupported { reason, .. } => {
                log::warn!(
                    "[qbz-qt] saved ALSA hardware-volume control no longer validates: {reason:?}"
                );
                set_hardware_volume_ui_probe(
                    device_id,
                    &probe,
                    None,
                    HardwareVolumeUiStatus::Disabled,
                );
            }
            HardwareVolumeDecision::Selected { .. } => {
                set_hardware_volume_ui_probe(
                    device_id,
                    &probe,
                    None,
                    HardwareVolumeUiStatus::Disabled,
                );
            }
        }
        publish_snapshot().await;
        return None;
    }

    match decision {
        HardwareVolumeDecision::Selected { selection } => {
            let Some(volume) = selection.candidate.current_volume else {
                log::warn!(
                    "[qbz-qt] selected ALSA control '{}' had no readable level",
                    selection.control
                );
                let _ = with_audio(|store| store.set_alsa_hardware_volume(false));
                set_hardware_volume_ui_probe(
                    device_id,
                    &probe,
                    Some(selection.control),
                    HardwareVolumeUiStatus::Unsupported,
                );
                apply_audio_with_owner(runtime, Apply::Reinit);
                return None;
            };
            let migrated = selection.source != HardwareVolumeSelectionSource::Persisted;
            if migrated {
                if let Err(error) = with_audio(|store| {
                    store.set_alsa_hardware_volume_control(
                        selection.route_key.clone(),
                        selection.control.clone(),
                    )
                }) {
                    log::error!(
                        "[qbz-qt] failed to migrate ALSA hardware-volume selection: {error}"
                    );
                    let _ = with_audio(|store| store.set_alsa_hardware_volume(false));
                    set_hardware_volume_ui_probe(
                        device_id,
                        &probe,
                        Some(selection.control),
                        HardwareVolumeUiStatus::Unsupported,
                    );
                    apply_audio_with_owner(runtime, Apply::Reinit);
                    return None;
                }
            }
            set_hardware_volume_ui_probe(
                device_id,
                &probe,
                Some(selection.control.clone()),
                HardwareVolumeUiStatus::Selected,
            );
            seed_hardware_volume(runtime, &selection.control, volume);
            if migrated {
                // Legacy `alsa_hardware_volume=true` had no exact identity in
                // the Player's startup snapshot. Reload the newly migrated map
                // without reopening the protected PCM route.
                apply_audio_with_owner(runtime, Apply::Reload);
            } else {
                publish_snapshot().await;
            }
            Some(volume)
        }
        HardwareVolumeDecision::NeedsChoice { reason, .. } => {
            log::warn!(
                "[qbz-qt] persisted ALSA hardware volume needs an explicit choice ({reason:?}); disabling without writing"
            );
            if let Err(error) = with_audio(|store| store.set_alsa_hardware_volume(false)) {
                log::error!("[qbz-qt] failed to disable ambiguous hardware volume: {error}");
                return None;
            }
            set_hardware_volume_ui_probe(
                device_id,
                &probe,
                None,
                hardware_volume_choice_status(reason),
            );
            apply_audio_with_owner(runtime, Apply::Reinit);
            None
        }
        HardwareVolumeDecision::Unsupported { reason, .. } => {
            log::warn!(
                "[qbz-qt] persisted ALSA hardware volume is unavailable; disabling: {reason:?}"
            );
            if let Err(error) = with_audio(|store| store.set_alsa_hardware_volume(false)) {
                log::error!("[qbz-qt] failed to disable unavailable hardware volume: {error}");
                return None;
            }
            set_hardware_volume_ui_probe(
                device_id,
                &probe,
                None,
                hardware_volume_unsupported_status(&reason),
            );
            apply_audio_with_owner(runtime, Apply::Reinit);
            None
        }
    }
}

pub(crate) async fn reconcile_alsa_hardware_volume(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
) -> Option<f32> {
    // Reconciliation can await a mixer probe, seed the Player's live volume and
    // reinitialize its output. One permit covers that whole sequence.
    let _owner_action = begin_audio_owner_action("reconcile hardware volume", true)?;
    reconcile_alsa_hardware_volume_with_owner(runtime).await
}

// ---------------------------------------------------------------------------
// Handlers (settings.rs handle_bool / handle_select / handle_slider /
// handle_string, including the cascades)
// ---------------------------------------------------------------------------

pub async fn settings_bool(runtime: &Arc<AppRuntime<LoggingAdapter>>, key: &str, value: bool) {
    if key == "playback-cache-dynamic" {
        update_playback_cache(|p| { p.make_custom(); p.dynamic = value; Ok(()) });
        publish_snapshot().await;
        return;
    }

    // Hardware-volume toggles can await a physical mixer write, seed the live
    // Player and reinitialize its output. Admit before the first persistence or
    // cascade so a rejected delegated action is a complete no-op.
    let _hardware_volume_owner_action = if key == "alsa-hardware-volume" {
        let Some(owner_action) = begin_audio_owner_action("change hardware volume", false) else {
            return;
        };
        Some(owner_action)
    } else {
        None
    };

    // Cross-setting cascades (settings.rs) — force dependents off first.
    let mut cascaded = false;
    match key {
        "dac-passthrough" if value => {
            if with_audio(|s| s.set_skip_sink_switch(false)).is_ok() {
                cascaded = true;
            }
        }
        "dac-passthrough" => {
            if with_audio(|s| s.set_pw_force_bitperfect(false)).is_ok() {
                cascaded = true;
            }
        }
        _ => {}
    }

    let outcome: Result<Apply, String> = match key {
        "limit-quality-to-device" => {
            with_audio(|s| s.set_limit_quality_to_device(value)).map(|_| Apply::Reload)
        }
        "alsa-hardware-volume" => set_alsa_hardware_volume(runtime, value).await,
        "exclusive-mode" => with_audio(|s| s.set_exclusive_mode(value)).map(|_| Apply::Reinit),
        "reserve-dac" => {
            with_audio(|s| s.set_reserve_dac_while_running(value)).map(|_| Apply::Reload)
        }
        "dac-passthrough" => with_audio(|s| s.set_dac_passthrough(value)).map(|_| Apply::Reinit),
        "pw-force-bitperfect" => {
            with_audio(|s| s.set_pw_force_bitperfect(value)).map(|_| Apply::Reload)
        }
        "allow-quality-fallback" => {
            with_audio(|s| s.set_allow_quality_fallback(value)).map(|_| Apply::Reload)
        }
        "sync-audio-on-startup" => {
            with_audio(|s| s.set_sync_audio_on_startup(value)).map(|_| Apply::Reload)
        }
        "skip-sink-switch" => with_audio(|s| s.set_skip_sink_switch(value)).map(|_| Apply::Reinit),
        "gapless" => with_audio(|s| s.set_gapless_enabled(value)).map(|_| Apply::Reload),
        "normalization" => {
            with_audio(|s| s.set_normalization_enabled(value)).map(|_| Apply::Reload)
        }
        "normalization-prevent-clipping" => {
            with_audio(|s| s.set_normalization_prevent_clipping(value)).map(|_| Apply::Reload)
        }
        "stream-uncached" => with_audio(|s| s.set_stream_first_track(value)).map(|_| Apply::Reload),
        "streaming-only" => with_audio(|s| s.set_streaming_only(value)).map(|_| Apply::Reload),
        "continue-playback" => {
            let mode = if value {
                AutoplayMode::ContinueWithinSource
            } else {
                AutoplayMode::PlayTrackOnly
            };
            with_playback(|s| s.set_autoplay_mode(mode)).map(|_| Apply::None)
        }
        "show-context-icon" => {
            let r = with_playback(|s| s.set_show_context_icon(value)).map(|_| Apply::None);
            // Republish: the bar reads this to decide whether the context glyph
            // shows, so persisting alone left the toggle inert until a restart.
            crate::now_playing::publish_current();
            r
        }
        "persist-session" => {
            // Push the LIVE gate too, not just the stored bool: capture/restore
            // read the gate, so persisting alone left the toggle inert until a
            // restart — the "renders, persists, drives nothing" class again.
            let r = with_playback(|s| s.set_persist_session(value)).map(|_| Apply::None);
            qbz_app::session_persist::set_persist_gate(value);
            r
        }
        "resume-position" => {
            let r = with_playback(|s| s.set_resume_playback_position(value)).map(|_| Apply::None);
            qbz_app::session_persist::set_resume_gate(value);
            r
        }
        // --- Appearance (phase 19): plain ui_prefs bools (+ live bridge
        // side-effects where the POC already has the consumer).
        "album-header-gradient" => {
            save_pref("album_header_gradient", serde_json::json!(value));
            Ok(Apply::None)
        }
        "compact-album-header" => {
            save_pref("compact_album_header", serde_json::json!(value));
            Ok(Apply::None)
        }
        "intelligent-search" => {
            save_pref("intelligent_search", serde_json::json!(value));
            // Flip the LIVE kill switch too, not just the pref: the service is
            // bound once per session (search_qt::init) and reads its own flag,
            // so persisting alone left the Settings row inert until the next
            // launch while the app-menu toggle (which does call this) took
            // effect immediately — two switches, one setting, different
            // behaviour.
            crate::search_qt::set_enabled(value);
            crate::search_bridge::ui(move |mut b| b.as_mut().set_intelligent_search(value));
            Ok(Apply::None)
        }
        // Section-nav placement (Slint ShellState.nav-in-sidebar /
        // .nav-header-compact). Both apply LIVE — Sidebar.qml mounts/unmounts
        // its nav block and HeaderBar.qml swaps tabs <-> compact icons off the
        // bridge properties, so the pref push below IS the apply step.
        "nav-in-sidebar" => {
            save_pref("nav_in_sidebar", serde_json::json!(value));
            crate::shell_bridge::ui(move |mut b| b.as_mut().set_nav_in_sidebar(value));
            Ok(Apply::None)
        }
        "nav-header-compact" => {
            save_pref("nav_header_compact", serde_json::json!(value));
            crate::shell_bridge::ui(move |mut b| b.as_mut().set_nav_header_compact(value));
            Ok(Apply::None)
        }
        // Collapsible My QBZ sidebar opt-ins. Persisted here; the sidebar reads
        // them live from the republished settingsJson (Phase 3 wires the tree).
        "collapsible-myqbz" => {
            save_pref("collapsible_myqbz", serde_json::json!(value));
            // When turned ON, populate the tree now: `publish_sidebar_tree()`
            // no-ops while the opt-in is OFF, so without this the sidebar would
            // stay empty until the next mixtape mutation. (Turning OFF needs no
            // push — the sidebar hides the tree off the republished settings.)
            if value {
                crate::myqbz_qt::publish_sidebar_tree();
            }
            Ok(Apply::None)
        }
        "myqbz-show-mixtapes" => {
            save_pref("myqbz_show_mixtapes", serde_json::json!(value));
            Ok(Apply::None)
        }
        "myqbz-show-collections" => {
            save_pref("myqbz_show_collections", serde_json::json!(value));
            Ok(Apply::None)
        }
        "hide-standard-myqbz" => {
            save_pref("hide_standard_myqbz", serde_json::json!(value));
            Ok(Apply::None)
        }
        // Sidebar-tree collapse state — persisted and read live from the
        // republished settingsJson by Sidebar.qml (no bridge property needed).
        "sidebar-playlists-collapsed" => {
            save_pref("sidebar_playlists_collapsed", serde_json::json!(value));
            Ok(Apply::None)
        }
        "myqbz-collapsed" => {
            save_pref("myqbz_collapsed", serde_json::json!(value));
            Ok(Apply::None)
        }
        "myqbz-mixtapes-collapsed" => {
            save_pref("myqbz_mixtapes_collapsed", serde_json::json!(value));
            Ok(Apply::None)
        }
        "myqbz-collections-collapsed" => {
            save_pref("myqbz_collections_collapsed", serde_json::json!(value));
            Ok(Apply::None)
        }
        // Appearance > Theme fieldsets, read back from settingsJson.
        "theme-custom-collapsed" => {
            save_pref("theme_custom_collapsed", serde_json::json!(value));
            Ok(Apply::None)
        }
        "theme-auto-collapsed" => {
            save_pref("theme_auto_collapsed", serde_json::json!(value));
            Ok(Apply::None)
        }
        "ambient-options-collapsed" => {
            save_pref("ambient_options_collapsed", serde_json::json!(value));
            Ok(Apply::None)
        }
        "sidebar-playlist-collage" => {
            save_pref("sidebar_playlist_collage", serde_json::json!(value));
            // LIVE: the sidebar rows swap collage <-> list-music glyph off the
            // bridge property. Without this push the row persisted a pref
            // nothing read — a Settings toggle that did nothing.
            crate::shell_bridge::ui(move |mut b| b.as_mut().set_sidebar_playlist_collage(value));
            Ok(Apply::None)
        }
        // These two are read live from the republished `settingsJson` by
        // QueuePanel / LibraryView. Separate keys are intentional: the queue
        // is tiny, while Library can contain thousands of track rows.
        "queue-track-artwork" => {
            save_pref("queue_track_artwork", serde_json::json!(value));
            Ok(Apply::None)
        }
        "library-track-artwork" => {
            save_pref("library_track_artwork", serde_json::json!(value));
            Ok(Apply::None)
        }
        "local-library-track-artwork" => {
            save_pref("local_library_track_artwork", serde_json::json!(value));
            // Republish so the local lists repaint live; the bridge property is
            // read at boot otherwise and the toggle would need a restart.
            crate::local_album_actions::publish_track_artwork();
            Ok(Apply::None)
        }
        "show-featured-artists" => {
            save_pref("show_featured_artists", serde_json::json!(value));
            Ok(Apply::None)
        }
        "play-indicator-animation" => {
            save_pref("play_indicator_animation", serde_json::json!(value));
            Ok(Apply::None)
        }
        "seekbar-waveform" => {
            save_pref("seekbar_waveform", serde_json::json!(value));
            qbz_audio::set_seek_waveform_enabled(value);
            crate::shell_bridge::ui(move |mut shell| shell.as_mut().set_seekbar_waveform(value));
            Ok(Apply::None)
        }
        "invert-swipe-navigation" => {
            save_pref("invert_swipe_navigation", serde_json::json!(value));
            // LIVE — NavGestureLayer reads it per gesture.
            crate::shell_bridge::ui(move |mut b| b.as_mut().set_invert_swipe_navigation(value));
            Ok(Apply::None)
        }
        "in-app-toasts" => {
            save_pref("in_app_toasts", serde_json::json!(value));
            Ok(Apply::None)
        }
        "system-notifications" => {
            save_pref("system_notifications", serde_json::json!(value));
            if !value {
                crate::spawn(qbz_media_controls::withdraw_track_notification());
            }
            Ok(Apply::None)
        }
        "window-title-show" => {
            save_pref("window_title_show", serde_json::json!(value));
            // LIVE — Main.qml's `title` binding re-evaluates (app.slint:44).
            crate::shell_bridge::ui(move |mut b| b.as_mut().set_window_title_show(value));
            Ok(Apply::None)
        }
        "use-system-title-bar" => {
            save_pref("use_system_title_bar", serde_json::json!(value));
            // Restart semantics (1:1 Slint): the APPLIED mode is untouched;
            // only the menu/row state flips live.
            log::info!("[qbz-qt] use_system_title_bar -> {value} (applies on next launch)");
            crate::shell_bridge::ui(move |mut b| b.as_mut().set_system_title_bar_pref(value));
            Ok(Apply::None)
        }
        "hide-title-bar" => {
            save_pref("hide_title_bar", serde_json::json!(value));
            // LIVE (no restart): this one only layers in-app — it drops the
            // drawn cluster AND the drag surface, per the reference's
            // `chrome-drag-enabled` (HeaderBar.slint:594-596).
            crate::shell_bridge::ui(move |mut b| b.as_mut().set_hide_title_bar(value));
            Ok(Apply::None)
        }
        "show-window-controls" => {
            save_pref("show_window_controls", serde_json::json!(value));
            crate::shell_bridge::ui(move |mut b| b.as_mut().set_show_window_controls(value));
            Ok(Apply::None)
        }
        "show-skip-ten" => {
            save_pref("show_skip_ten", serde_json::json!(value));
            // Live mirror on the domain bridge (the §5.6 pattern): the bar
            // reads QbzPlayer.showSkipTen, not the settings document.
            crate::player_bridge::ui(move |mut b| b.as_mut().set_show_skip_ten(value));
            Ok(Apply::None)
        }
        "show-volume-steppers" => {
            save_pref("show_volume_steppers", serde_json::json!(value));
            Ok(Apply::None)
        }
        "show-purchases" => {
            save_pref("show_purchases", serde_json::json!(value));
            Ok(Apply::None)
        }
        // Read by BOTH nav hosts off `settingsJson` (shell/NavFlyout.qml), so
        // there is nothing to apply here beyond the write and the republish
        // every settings toggle already does.
        "nav-click-first-tab" => {
            save_pref("nav_click_first_tab", serde_json::json!(value));
            Ok(Apply::None)
        }
        "tray-enable" => tray()
            .set_enable_tray(value)
            .map_err(|e| e.to_string())
            .map(|_| Apply::None),
        "tray-close-to-tray" => {
            let persisted = tray().set_close_to_tray(value).map_err(|e| e.to_string());
            // A-21b(c), and the FIFTH live-update row of §5.6. Without this the
            // `QbzTray.closeToTray` property never leaves the value it was
            // seeded with and the toggle is COSMETIC until the next launch —
            // the "renders and persists and drives nothing" defect this whole
            // diff exists to end. The line above is the storage; this is the
            // running app, and after it the very next close behaves the new way.
            //
            // Gated on the persist: a failed write (no active session) must not
            // tell the running app a value the store does not hold.
            if persisted.is_ok() {
                crate::tray_bridge::ui(move |mut t| {
                    t.as_mut().set_close_to_tray(value);
                });
            }
            persisted.map(|_| Apply::None)
        }
        // STORAGE ONLY (owner ruling K5): no Settings row, no reader. The value
        // round-trips through the per-user store the Slint build shares instead
        // of a Qt session dropping it — see the doc field for why the reference
        // hides the toggle.
        "tray-minimize-to-tray" => tray()
            .set_minimize_to_tray(value)
            .map_err(|e| e.to_string())
            .map(|_| Apply::None),
        "tray-mac-hide-dock" => tray()
            .set_mac_hide_dock(value)
            .map_err(|e| e.to_string())
            .map(|_| Apply::None),
        // --- Integrations (phase 19) --------------------------------------
        "show-recommendations" => {
            crate::integrations_qt::set_show_recommendations(value).map(|_| Apply::None)
        }
        "musicbrainz" => {
            match crate::integrations_qt::set_musicbrainz_enabled(value) {
                Ok(()) => {
                    // Live apply (main.rs:9019-9029 seeds core from this pref).
                    runtime.core().musicbrainz_set_enabled(value).await;
                    // ...and repaint the open artist page, because every
                    // MB-derived field on that document is baked when the
                    // document is built: `mbAvailable`, the Origin block, the
                    // relationship rows, and `origin.locationClickable` — the
                    // gate on both Artist Scene doors. Without this, turning
                    // MusicBrainz OFF leaves a live "Artist Scene" link on a
                    // page the user navigates back to, and following it would
                    // run discovery against a disabled client that reports a
                    // false "no artists found" rather than an error.
                    crate::republish_open_artist();
                    Ok(Apply::None)
                }
                Err(e) => Err(e),
            }
        }
        "scrobble-enable" => {
            crate::integrations_qt::set_scrobble_enabled(value).map(|_| Apply::None)
        }
        "scrobble-collapse" => {
            crate::integrations_qt::set_scrobble_collapsed(value).map(|_| Apply::None)
        }
        "scrobble-logged-out" => {
            crate::integrations_qt::set_logged_out_scrobbling(value).map(|_| Apply::None)
        }
        // --- Privacy: the local listen log ------------------------------
        "listen-history" => crate::listen_log_qt::set_enabled(value)
            .await
            .map(|_| Apply::None),
        "lastfm-enable" => crate::integrations_qt::set_lastfm_enabled(value).map(|_| Apply::None),
        "listenbrainz-enable" => {
            crate::integrations_qt::set_listenbrainz_enabled(value).map(|_| Apply::None)
        }
        "discord-rpc" => crate::integrations_qt::set_discord_enabled(value).map(|_| Apply::None),
        // The OS-level `qobuzapp://` handler claim (link_handler_qt).
        "qobuz-links" => crate::link_handler_qt::set_enabled(value).map(|_| Apply::None),
        // --- Offline -------------------------------------------------------
        // Induced offline. The engine takes the #279 stream-first snapshot,
        // so the audio settings can change under us -> Reload the player.
        "offline-mode-enabled" => offline::set_mode_enabled(value).map(|_| Apply::Reload),
        "offline-scrobble-immediate" => {
            offline::set_allow_immediate_scrobbling(value).map(|_| Apply::None)
        }
        "offline-scrobble-accumulated" => {
            offline::set_allow_accumulated_scrobbling(value).map(|_| Apply::None)
        }
        // --- Network ---------------------------------------------------------
        "network-proxy-enabled" => network::set_proxy_enabled(value).map(|_| Apply::None),
        "network-block-qconnect-lan" => {
            network::set_block_qconnect_lan(value).map(|_| Apply::None)
        }
        "network-block-cast" => network::set_block_cast(value).map(|_| Apply::None),
        // --- Local Library > Plex -----------------------------------------
        "plex-metadata-write" => {
            library::set_metadata_write(value);
            Ok(Apply::None)
        }
        "plex-collapse" => {
            save_pref("plex_ui_collapsed", serde_json::json!(value));
            Ok(Apply::None)
        }
        // The media servers keep their collapse state in their OWN store
        // rather than in ui_prefs, because the whole row lives there — a
        // second home for one boolean is how the two drift out of step on a
        // user switch.
        "jellyfin-collapse" | "subsonic-collapse" => {
            use qbz_app::settings::media_servers::MediaServerKind;
            let kind = if key.starts_with("jellyfin") {
                MediaServerKind::Jellyfin
            } else {
                MediaServerKind::Subsonic
            };
            let mut cfg = crate::media_servers_qt::get(kind);
            cfg.ui_collapsed = value;
            crate::media_servers_qt::put(kind, &cfg);
            Ok(Apply::None)
        }
        other => {
            log::warn!("[qbz-qt] unknown settings bool key: {other}");
            return;
        }
    };
    match outcome {
        Ok(apply) => {
            let apply = if cascaded { Apply::Reinit } else { apply };
            // #638 fix 3, trigger 2 — the "Limit quality to device" toggle.
            // AFTER the persist above (the probe re-reads the flag) and BEFORE
            // `apply_audio`, which publishes the document on its own way out;
            // see `refresh_device_cap` for why that ordering is load-bearing.
            if key == "limit-quality-to-device" {
                refresh_device_cap(runtime).await;
            }
            if key == "alsa-hardware-volume" {
                apply_audio_with_owner(runtime, apply);
                force_bitperfect_volume_with_owner(runtime).await;
            } else {
                apply_audio(runtime, apply);
            }
            publish_snapshot().await;
        }
        Err(e) => {
            log::error!("[qbz-qt] settings persist failed ({key}): {e}");
            if key == "alsa-hardware-volume" {
                // The failed enable path persisted `false`; push that state to
                // the player/document and explain why the toggle bounced back.
                apply_audio_with_owner(runtime, Apply::Reinit);
                force_bitperfect_volume_with_owner(runtime).await;
                publish_snapshot().await;
                crate::toast_qt::error(qbz_i18n::t(
                    "This ALSA device has no compatible hardware volume control. Direct playback remains fixed at 100%.",
                ));
            }
        }
    }
}

async fn select_alsa_hardware_volume_control(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    index: usize,
) {
    let audio = match with_audio(|store| store.get_settings()) {
        Ok(audio) => audio,
        Err(error) => {
            log::error!("[qbz-qt] read audio settings for mixer selection failed: {error}");
            return;
        }
    };
    if !qbz_audio::alsa_direct::uses_alsa_direct_route(&audio) {
        log::warn!("[qbz-qt] ignored mixer selection outside ALSA Direct");
        return;
    }
    // Selection validation can write a mixer control and seed live volume.
    // Refuse before the async probe so it cannot straddle a handoff.
    let Some(_owner_action) = begin_audio_owner_action("select hardware volume control", true)
    else {
        return;
    };
    let device_id = audio
        .output_device
        .clone()
        .expect("ALSA Direct route has a device id");
    let requested = HARDWARE_VOLUME_UI.lock().ok().and_then(|state| {
        (state.device_id.as_deref() == Some(device_id.as_str()))
            .then(|| {
                state
                    .candidates
                    .get(index)
                    .map(|candidate| candidate.id.clone())
            })
            .flatten()
    });
    let Some(requested) = requested else {
        log::warn!("[qbz-qt] ignored stale ALSA mixer option index {index}");
        publish_snapshot().await;
        return;
    };

    set_hardware_volume_ui_status(Some(device_id.clone()), HardwareVolumeUiStatus::Probing);
    publish_snapshot().await;

    let (probe, _) = match probe_selected_hardware_volume(&audio).await {
        Ok(result) => result,
        Err(error) => {
            log::warn!("[qbz-qt] ALSA mixer revalidation failed: {error}");
            let _ = with_audio(|store| store.set_alsa_hardware_volume(false));
            set_hardware_volume_ui_status(Some(device_id), HardwareVolumeUiStatus::Unsupported);
            apply_audio_with_owner(runtime, Apply::Reinit);
            publish_snapshot().await;
            return;
        }
    };
    if let Err(error) = current_hardware_volume_route_is(&device_id) {
        log::warn!("[qbz-qt] discarded stale ALSA mixer selection: {error}");
        publish_snapshot().await;
        return;
    }
    let candidate = probe
        .valid_candidates()
        .find(|candidate| candidate.id == requested)
        .cloned();
    let Some(candidate) = candidate else {
        log::warn!(
            "[qbz-qt] ALSA mixer control '{}' disappeared before selection; no fallback chosen",
            requested
        );
        let _ = with_audio(|store| store.set_alsa_hardware_volume(false));
        set_hardware_volume_ui_probe(device_id, &probe, None, HardwareVolumeUiStatus::Stale);
        apply_audio_with_owner(runtime, Apply::Reinit);
        publish_snapshot().await;
        return;
    };
    let Some(route_key) = probe.route_key.clone() else {
        log::warn!("[qbz-qt] ALSA mixer selection has no stable route identity");
        let _ = with_audio(|store| store.set_alsa_hardware_volume(false));
        set_hardware_volume_ui_probe(device_id, &probe, None, HardwareVolumeUiStatus::Unsupported);
        apply_audio_with_owner(runtime, Apply::Reinit);
        publish_snapshot().await;
        return;
    };
    let selection = HardwareVolumeSelection {
        route_key,
        control: requested,
        candidate,
        source: HardwareVolumeSelectionSource::UserChoice,
    };
    match enable_hardware_volume_selection(runtime, device_id, probe, selection).await {
        Ok(apply) => {
            apply_audio_with_owner(runtime, apply);
            force_bitperfect_volume_with_owner(runtime).await;
        }
        Err(error) => {
            log::error!("[qbz-qt] persist ALSA mixer selection failed: {error}");
            let _ = with_audio(|store| store.set_alsa_hardware_volume(false));
            apply_audio_with_owner(runtime, Apply::Reinit);
        }
    }
    publish_snapshot().await;
}

/// Backend-switch cascade for `exclusive_mode`: the flag survives a switch
/// only onto a backend that honours it. That used to read "ALSA alone", which
/// on macOS wiped the CoreAudio Hog Mode setting every time Auto / System
/// default was re-selected — the #748 reporter's DB workaround died to
/// exactly this. One predicate with the Settings row and the EXCL LED
/// (`output_labels::backend_honours_exclusive`).
fn backend_switch_clears_exclusive(backend: AudioBackendType) -> bool {
    !crate::output_labels::backend_honours_exclusive(Some(backend), cfg!(target_os = "macos"))
}

pub async fn settings_select(runtime: &Arc<AppRuntime<LoggingAdapter>>, key: &str, index: usize) {
    match key {
        "alsa-hardware-volume-control" => {
            select_alsa_hardware_volume_control(runtime, index).await;
        }
        "streaming-quality" => {
            let Some(key) = STREAMING_QUALITY_KEYS.get(index) else {
                return;
            };
            save_streaming_quality(key);
            // Apply to the playback request tier + drop the tier-keyed cache
            // (settings.rs: bytes fetched at the old tier must not keep
            // serving).
            crate::playback_qt::set_streaming_quality(key);
            log::info!("[qbz-qt] streaming quality changed -> clearing audio cache");
            runtime.core().player().clear_audio_cache();
        }
        "backend" => {
            let Some(_owner_action) = begin_audio_owner_action("change audio backend", false)
            else {
                return;
            };
            // Index 0 = "Auto" (resolve-and-set, #470): PipeWire when present,
            // else System. Indices >= 1 map to the concrete backends list.
            let backend = if index == 0 {
                let types = MAPS.lock().unwrap().0.clone();
                if types.iter().any(|t| *t == AudioBackendType::PipeWire) {
                    AudioBackendType::PipeWire
                } else {
                    AudioBackendType::SystemDefault
                }
            } else {
                let types = MAPS.lock().unwrap().0.clone();
                match types.get(index - 1) {
                    Some(t) => *t,
                    None => return,
                }
            };
            let previous_backend = audio_settings().backend_type.unwrap_or_default();
            if previous_backend == AudioBackendType::Alsa && backend != AudioBackendType::Alsa {
                // This must complete BEFORE the new backend is persisted and
                // initialized. Reinit used to drop the ALSA PCM but omitted
                // the suspended-sink cleanup, while Refresh merely queued a
                // release and raced its own re-enumeration.
                if let Err(error) = release_output_device_with_owner(runtime).await {
                    report_release_failure(&error);
                }
            }
            if let Err(e) = with_audio(|s| s.set_backend_type(Some(backend))) {
                log::error!("[qbz-qt] persist backend failed: {e}");
                return;
            }
            // Backend-switch cascade (settings.rs): routing-critical toggles
            // that don't translate across stacks reset.
            if backend != AudioBackendType::PipeWire {
                let _ = with_audio(|s| s.set_dac_passthrough(false));
                let _ = with_audio(|s| s.set_pw_force_bitperfect(false));
            }
            if backend_switch_clears_exclusive(backend) {
                let _ = with_audio(|s| s.set_exclusive_mode(false));
            }
            // GAPLESS IS DELIBERATELY NOT CASCADED — owner decision, 2026-07-31:
            // "hay que mantener el status activo, sin importar el cambio en el
            // backend". It keeps whatever the user set, across every backend
            // change, ALSA included.
            //
            // This diverges from BOTH references on purpose. Tauri forced it off
            // when moving to ALSA (`SettingsView.svelte:3409-3416`, "not
            // compatible with ALSA Direct") and Slint copied that
            // (`settings.rs:1232-1237`); in the engine's own vocabulary
            // `using_alsa_direct` IS `backend_type == Alsa`
            // (qbz-player/src/player/mod.rs:725-729), so the two agree.
            //
            // The exclusion looks broader than the hardware demands: the
            // engine's `PlayNext` handler is backend-AGNOSTIC — it appends to
            // the live engine and refuses only on a missing engine, a
            // sample-rate/channel mismatch, or an active streaming source
            // (mod.rs:3553-3586) — and ALSA Direct does have an engine with its
            // own writer thread. Within one album at a constant rate there is no
            // mechanism here that should break.
            //
            // What this port had was WORSE than either reference: the guard was
            // missing entirely, so EVERY backend change killed gapless,
            // including switching to PipeWire where it works perfectly. That is
            // what read as "alsa breaks gapless" — the switch turned it off.
            //
            // The failure mode to watch, if it ever does misbehave on ALSA, is a
            // transition that renegotiates the stream mid-album (a rate change);
            // the format-match guard above should already refuse those.
            let _ = with_audio(|s| s.set_output_device(None));
            // #638 fix 3, trigger 3 — a backend switch resets the device to
            // the system default, so the cap's subject changed even though the
            // device SELECTION did not. D7 also makes the new backend the one
            // that resolves that default.
            refresh_device_cap(runtime).await;
            apply_audio_with_owner(runtime, Apply::Reinit);
            force_bitperfect_volume_with_owner(runtime).await;
        }
        "device" => {
            let Some(_owner_action) = begin_audio_owner_action("change output device", false)
            else {
                return;
            };
            let id = {
                let ids = MAPS.lock().unwrap().1.clone();
                ids.get(index).cloned()
            };
            let Some(id) = id else {
                return;
            };
            let device_opt = if id.is_empty() {
                None
            } else {
                Some(id.as_str())
            };
            if let Err(e) = with_audio(|s| s.set_output_device(device_opt)) {
                log::error!("[qbz-qt] persist output device failed: {e}");
                return;
            }
            // #638 fix 3, trigger 4 — a different DAC has a different ceiling.
            refresh_device_cap(runtime).await;
            reconcile_alsa_hardware_volume_with_owner(runtime).await;
            apply_audio_with_owner(runtime, Apply::Reinit);
        }
        "dsd-mode" => {
            let Some(mode) = DSD_MODE_VALUES.get(index) else {
                return;
            };
            if let Err(e) = with_audio(|s| s.set_dsd_mode(mode)) {
                log::error!("[qbz-qt] persist dsd mode failed: {e}");
                return;
            }
            apply_audio(runtime, Apply::Reinit);
        }
        "alsa-plugin" => {
            let Some(_owner_action) = begin_audio_owner_action("change ALSA plugin", false) else {
                return;
            };
            let Some(plugin) = ALSA_PLUGIN_VALUES.get(index).copied() else {
                return;
            };
            if let Err(e) = with_audio(|s| s.set_alsa_plugin(Some(plugin))) {
                log::error!("[qbz-qt] persist alsa plugin failed: {e}");
                return;
            }
            reconcile_alsa_hardware_volume_with_owner(runtime).await;
            apply_audio_with_owner(runtime, Apply::Reinit);
            force_bitperfect_volume_with_owner(runtime).await;
        }
        "retry-behavior" => {
            let Some(behavior) = RETRY_BEHAVIOR_VALUES.get(index) else {
                return;
            };
            if let Err(e) = with_audio(|s| s.set_quality_fallback_behavior(behavior)) {
                log::error!("[qbz-qt] persist retry behavior failed: {e}");
                return;
            }
            apply_audio(runtime, Apply::Reload);
        }
        "normalization-target" => {
            let Some((_, lufs)) = NORMALIZATION_TARGETS.get(index) else {
                return; // the trailing "Custom" entry is display-only
            };
            if let Err(e) = with_audio(|s| s.set_normalization_target_lufs(*lufs)) {
                log::error!("[qbz-qt] persist normalization target failed: {e}");
                return;
            }
            apply_audio(runtime, Apply::Reload);
        }
        "qconnect-startup" => {
            let Some(mode) = QCONNECT_STARTUP_VALUES.get(index) else {
                return;
            };
            if let Some(mode) = qconnect_app::QconnectStartupMode::from_str(mode) {
                crate::qconnect_transport_qt::save_startup_mode(mode);
            }
        }
        "qconnect-conflict-policy" => {
            let Some(policy) = qconnect_app::QconnectPlaybackConflictPolicy::from_index(index)
            else {
                return;
            };
            crate::qconnect_transport_qt::save_playback_conflict_policy(policy);
            crate::qconnect_qt::publish::playback_conflict_policy_index(policy.index());
            log::info!(
                "[QConnect] playback conflict policy changed to {}",
                policy.as_str()
            );
        }
        // --- Appearance (phase 19) ----------------------------------------
        "app-background" => {
            let Some(mode) = APP_BACKGROUND_VALUES.get(index) else {
                return;
            };
            save_pref("app_background", serde_json::json!(mode));
            // Live (pure QML layering — AppShell mounts AmbientField for 1 and
            // ImmersiveAtmosphere for 2, exactly like AppShell.slint:213-231).
            let ambient = index as i32;
            crate::shell_bridge::ui(move |mut b| b.as_mut().set_ambient_mode(ambient));
            // The wallpaper modes need their image (a no-op for the others).
            crate::wallpaper_qt::refresh();
        }
        "language" => {
            let Some(lang) = LANGUAGE_VALUES.get(index) else {
                return;
            };
            save_pref("language", serde_json::json!(lang));
            // Live switch (phase 20): trRev bump + doc republish.
            crate::apply_language(lang.to_string());
        }
        "app-font" => {
            let Some(font) = APP_FONT_VALUES.get(index) else {
                return;
            };
            save_pref("app_font", serde_json::json!(font));
            // APPLIED AT THE NEXT START, and that is not a shortcut.
            //
            // The app's text is 1082 plain `Text` items, and a plain Text
            // takes the APPLICATION font at CONSTRUCTION — it does not follow
            // ApplicationWindow.font (Controls only) and it does not react to
            // QGuiApplication::setFont afterwards. Both were measured against
            // this Qt build before this arm was written; the notes are in
            // qml/FontPreload.qml. So a running app cannot be re-faced without
            // rebuilding every item, and the honest thing is to persist the
            // choice and let main() apply it on the next start, exactly like
            // the Interface size row two lines above.
            log::info!("[qbz-qt] app_font -> {font} (applies at next start)");
        }
        "ui-scale" => {
            let Some(scale) = UI_SCALE_VALUES.get(index) else {
                return;
            };
            save_ui_scale(scale);
            log::info!("[qbz-qt] ui_scale -> {scale} (applies at next start)");
        }
        "immersive-search-action" => {
            let Some(v) = IMMERSIVE_SEARCH_VALUES.get(index) else {
                return;
            };
            save_pref("immersive_search_action", serde_json::json!(v));
        }
        "immersive-default-view" => {
            let Some(v) = IMMERSIVE_VIEW_VALUES.get(index) else {
                return;
            };
            save_pref("immersive_default_view", serde_json::json!(v));
        }
        "wc-position" => {
            let Some(v) = WC_POSITION_VALUES.get(index) else {
                return;
            };
            save_pref("wc_position", serde_json::json!(v));
            // LIVE — HeaderBar re-anchors the cluster and flips its order.
            // The reference gets this for free (its settings view writes the
            // shared AppearanceState directly, main.rs:11265-11271 is
            // persist-only); here the push is the whole wiring.
            let on_left = *v == "left";
            crate::shell_bridge::ui(move |mut b| b.as_mut().set_wc_on_left(on_left));
        }
        "mini-default-view" => {
            let Some(v) = MINI_VIEW_VALUES.get(index) else {
                return;
            };
            save_pref("mini_default_view", serde_json::json!(v));
        }
        "startup-page" => {
            let Some(v) = STARTUP_PAGE_VALUES.get(index) else {
                return;
            };
            save_pref("startup_page", serde_json::json!(v));
        }
        "image-cache-max" => {
            let Some(mb) = offline::IMAGE_CACHE_MB.get(index) else {
                return;
            };
            save_pref("image_cache_max_mb", serde_json::json!(mb));
            // A smaller budget applies right away, not at the next boot.
            crate::artwork_qt::trim_shared_now();
        }
        "genre-filters-position" => {
            let Some(v) = LOCAL_GENRE_FILTER_POSITION_VALUES.get(index) else {
                return;
            };
            save_pref("local_genre_filters_position", serde_json::json!(v));
        }
        "renderer" => {
            let Some(v) = RENDERER_VALUES.get(index) else {
                return;
            };
            save_pref("renderer", serde_json::json!(v));
            log::info!("[qbz-qt] renderer -> {v} (restart to apply)");
            // The reference toasts here (`crates/qbz/src/main.rs:11326-11329`)
            // and this port only logged — so the row changed nothing visible
            // and gave no hint that a restart was needed.
            crate::toast_qt::info(qbz_i18n::t("Renderer changed — restart QBZ to apply"));
        }
        "gpu-power" => {
            // Index 0 is Auto; every other position is a real Qt-enumerated
            // device. Persist model + stable identity atomically, never the
            // process-local Qt Vulkan index.
            let gpus = crate::renderer_qt::gpus();
            let selected = if index <= 0 {
                None
            } else {
                gpus.get((index - 1) as usize)
            };
            save_gpu_preference(selected);
            let value = selected.map(|gpu| gpu.name.as_str()).unwrap_or("auto");
            log::info!("[qbz-qt] gpu_power -> {value} (restart to apply)");
            crate::toast_qt::info(qbz_i18n::t("Preferred GPU changed — restart QBZ to apply"));
        }
        "tray-icon-theme" => {
            let Some(v) = TRAY_ICON_VALUES.get(index) else {
                return;
            };
            if let Err(e) = tray().set_tray_icon_theme(v) {
                log::error!("[qbz-qt] persist tray icon theme failed: {e}");
            }
            // Re-theme the RUNNING tray icon live, no restart — the fourth of
            // §5.6's live-update rows (reference: `crates/qbz/src/main.rs:11258-11264`).
            // The push re-decodes the pixmaps on the updater thread and emits
            // the SNI `NewIcon` signal, so panels re-fetch in place. A silent
            // no-op when no tray is live, which is the whole point of routing
            // through `handle()`.
            if let Some(t) = crate::tray_qt::handle() {
                t.set_icon_theme(v.to_string());
            }
        }
        "auto-theme-source" => {
            const SOURCES: &[&str] = &["system", "wallpaper", "image"];
            let Some(v) = SOURCES.get(index) else {
                return;
            };
            save_pref("auto_theme_source", serde_json::json!(v));
            // Regenerate NOW if the auto theme is the one on screen. The
            // reference regenerates on activation, on source change, on image
            // pick and on the explicit button (auto_theme.rs header); this is
            // the "on source change" arm, and without it the row persisted a
            // source that only took effect on the next launch.
            if crate::theme_qt::current_slug() == "auto" {
                crate::theme_qt::publish_theme();
            }
        }
        other => log::warn!("[qbz-qt] unknown settings select key: {other}"),
    }
    publish_snapshot().await;
}

pub async fn settings_slider(runtime: &Arc<AppRuntime<LoggingAdapter>>, key: &str, value: i32) {
    if key == "wallpaper-blur" {
        // Persisted on the slider's release; the drag itself already moved
        // QbzShell.wallpaperBlur for the live preview.
        let blur = wallpaper_blur_from_percent(value);
        // Written from the whole percent as f64: an f32 widened on the way to
        // JSON would store 0.62 as 0.6200000047683716.
        save_pref("wallpaper_blur", serde_json::json!(f64::from(value.clamp(0, 100)) / 100.0));
        crate::shell_bridge::ui(move |mut b| b.as_mut().set_wallpaper_blur(blur));
    }
    if key == "buffer-seconds" {
        let seconds = value.clamp(1, 10) as u8;
        match with_audio(|s| s.set_stream_buffer_seconds(seconds)) {
            Ok(()) => apply_audio(runtime, Apply::Reload),
            Err(e) => log::error!("[qbz-qt] persist buffer seconds failed: {e}"),
        }
    }
    publish_snapshot().await;
}

fn update_playback_cache(change: impl FnOnce(&mut qbz_models::playback_cache::PlaybackCacheSettings) -> Result<(), String>) {
    let result = with_audio(|store| {
        let mut settings = store.get_settings()?;
        change(&mut settings.playback_cache)?;
        store.set_playback_cache(&settings.playback_cache)?;
        if let Some(runtime) = crate::APP.get() {
            runtime.core().player().configure_playback_cache(settings.playback_cache)?;
        }
        Ok(())
    });
    if let Err(error) = result {
        log::warn!("Playback cache setting rejected: {error}");
        crate::toast_qt::error(qbz_i18n::t(&error));
    }
}

/// String-payload handler. Also the ACTION channel for the sections whose
/// affordances are buttons rather than settings (Local Library folders and
/// scans, the caches, the developer tools): the payload is the action's
/// argument ("" when it takes none).
pub async fn settings_string(key: &str, value: String) {
    match key {
        "playback-memory-profile" => update_playback_cache(|policy| {
            policy.select_profile(qbz_models::playback_cache::PlaybackMemoryProfile::parse(&value)?);
            Ok(())
        }),
        "playback-cache-min" | "playback-cache-max" => {
            update_playback_cache(|policy| {
                let limit = if value.trim().is_empty() { None } else {
                    Some(value.trim().parse::<u32>().map_err(|_| "Playback cache limits must be between 16 and 16384 MiB".to_string())?)
                };
                policy.make_custom();
                if key == "playback-cache-min" { policy.min_mib = limit; } else { policy.max_mib = limit; }
                Ok(())
            });
        }
        "playback-cache-reset" => update_playback_cache(|policy| {
            policy.select_profile(qbz_models::playback_cache::PlaybackMemoryProfile::Auto); Ok(())
        }),
        "playback-cache-refresh" => {},
        "appearance-profile" => appearance_profile::apply(&value),
        "playback-storage-folder" => playback_storage::set(value).await,
        "playback-storage-browse" => playback_storage::browse().await,
        "playback-cache-apply" => {
            if let Err(error) = crate::playback_qt::apply_playback_memory(&value).await {
                log::warn!("[playback-memory] Apply failed: {error}");
                crate::toast_qt::error(qbz_i18n::t(&error));
            }
        },

        "qconnect-device-name" => {
            let trimmed = value.trim().to_string();
            let stored = (!trimmed.is_empty()).then_some(trimmed);
            crate::qconnect_transport_qt::persist_device_name(stored.as_deref());
            if let Some(service) = crate::qconnect_qt::service() {
                service.set_custom_device_name(stored).await;
            }
        }
        "myqbz-label" => save_myqbz_label(&value),
        "listenbrainz-token" => {
            // Validates + persists + republishes itself (async flow).
            crate::integrations_qt::listenbrainz_set_token(&value).await;
            return;
        }
        // --- Local Library -------------------------------------------------
        "local-tab-order" => {
            if !save_local_tab_order_payload(&value) {
                log::warn!("[qbz-qt] rejected malformed Local Library tab order");
            }
        }
        "local-tab-order-reset" => save_pref(
            "local_tab_order",
            serde_json::json!(LOCAL_TAB_DEFAULT_ORDER),
        ),
        "library-add-folder" => library::add_folder(value).await,
        // --- Per-folder settings modal (LibFolderEditModal) ------------------
        "library-folder-edit-open" => {
            if let Ok(id) = value.trim().parse::<i64>() {
                library::open_folder_edit(id).await;
            }
            return;
        }
        "library-folder-edit-close" => {
            library::close_folder_edit().await;
            return;
        }
        "library-folder-edit-save" => {
            // {id, alias, enabled, isNetwork, fsType, userOverrideNetwork}
            #[derive(serde::Deserialize)]
            struct SavePayload {
                id: i64,
                alias: String,
                enabled: bool,
                #[serde(rename = "isNetwork")]
                is_network: bool,
                #[serde(rename = "fsType")]
                fs_type: String,
                #[serde(rename = "userOverrideNetwork")]
                user_override_network: bool,
            }
            match serde_json::from_str::<SavePayload>(&value) {
                Ok(p) => {
                    library::save_folder_edit(
                        p.id,
                        p.alias,
                        p.enabled,
                        p.is_network,
                        p.fs_type,
                        p.user_override_network,
                    )
                    .await
                }
                Err(e) => log::warn!("[qbz-qt] folder-edit save payload: {e}"),
            }
            return;
        }
        "library-folder-change-path" => {
            if let Ok(id) = value.trim().parse::<i64>() {
                library::change_folder_path(id).await;
            }
            return;
        }
        // Appearance > Auto (dynamic) > "Select Image...". The native picker,
        // then persist BOTH the path and `source = image` before regenerating
        // — `theme_qt::auto_source` re-reads the prefs, so the order matters
        // (1:1 with `crates/qbz/src/auto_theme.rs:108-140`). Cancel is a no-op
        // with no toast, like every other picker in the port.
        //
        // The reader has been here the whole time (`theme_qt.rs:90` builds
        // `AutoSource::Image` from `auto_theme_image_path`); what was missing
        // was any way for a user to WRITE it, which made "Custom Image" a
        // source you could select and never supply.
        "auto-theme-select-image" => {
            let Some(file) = rfd::AsyncFileDialog::new()
                .set_title(&qbz_i18n::t("Select Image..."))
                .add_filter(
                    &qbz_i18n::t("Image"),
                    &["png", "jpg", "jpeg", "webp", "bmp", "tiff"],
                )
                .pick_file()
                .await
            else {
                return;
            };
            let path = file.path().to_string_lossy().to_string();
            save_pref("auto_theme_image_path", serde_json::json!(path));
            save_pref("auto_theme_source", serde_json::json!("image"));
            // Repaint now when the auto theme is the one on screen — the same
            // arm the source dropdown takes; picking an image and seeing
            // nothing change until the next launch is the defect that row
            // already had.
            if crate::theme_qt::current_slug() == "auto" {
                crate::theme_qt::publish_theme();
            }
        }
        "library-open-folders" => {
            // Settings > Local Library > Library folders > Manage. The folder
            // table is a full-page view now (owner 2026-08-21), reached the
            // same way the Blacklist manager is: record the route and let
            // ContentRouter mount it. `nav_qt::record` republishes canBack /
            // canForward / currentView in one hop, so Back works without any
            // further bookkeeping here.
            //
            // No load call beside it — unlike `blacklist_qt::open_manager`,
            // this view reads the SETTINGS document, which is already live,
            // and asks for its own "refresh" on mount.
            crate::nav_qt::record("libraryfolders");
        }
        "library-pick-folder" => {
            // Native chooser fills the field; Add confirms registration.
            library::pick_and_add_folder().await;
        }
        "library-remove-folders" => {
            let ids: Vec<i64> = serde_json::from_str(&value).unwrap_or_default();
            library::remove_folders(ids).await;
        }
        "library-folder-enabled" => {
            if let Ok(id) = value.trim().parse::<i64>() {
                library::toggle_folder_enabled(id).await;
            }
        }
        // "" = every enabled folder; "<id>" = that folder only.
        "library-scan" => {
            library::scan(value.trim().parse::<i64>().ok());
        }
        "library-scan-stop" => library::stop_scan(),
        // Panel mount / manual refresh: fall through to the publish below,
        // then probe the network mounts OFF that path (the publish must not
        // wait on a dead NFS mount — see library::spawn_accessibility_probes).
        "refresh" => {
            publish_snapshot().await;
            library::spawn_accessibility_probes().await;
            return;
        }
        "library-cleanup-missing" => {
            // Publishes its own progress + result.
            library::cleanup_missing().await;
            return;
        }
        "library-clear" => {
            library::clear_library().await;
            return;
        }
        "plex-clear-cache" => library::plex_clear_cache().await,
        // --- Offline --------------------------------------------------------
        "lyrics-cache-clear" => offline::clear_lyrics_cache().await,
        "image-cache-clear" => offline::clear_image_cache().await,
        // Offline > "Check now": nudge the connectivity actor. The status it
        // publishes flows back through offline_fwd's forwarder, so there is
        // nothing to await and nothing to republish here.
        "offline-recheck" => crate::offline_fwd::request_recheck(),
        // --- Network ---------------------------------------------------------
        "network-clear-password" => network::clear_password(),
        // --- Developer ------------------------------------------------------
        "open-log-file" => devtools::open_log_file(),
        // The value is the include-auth gate ("with-auth" or empty) — see
        // DeveloperSettings.qml for why it is session state and not a pref.
        // --- Import / Export ---------------------------------------------
        "export-settings" => import_export::export_settings(value == "with-auth").await,
        "export-blacklist" => import_export::export_blacklist().await,
        "import-blacklist" => import_export::import_blacklist().await,
        "account-snapshot" => import_export::create_snapshot().await,
        "account-delete-snapshot" => import_export::delete_snapshot(value).await,
        "account-migrate" => import_export::migrate(value).await,
        other => log::warn!("[qbz-qt] unknown settings string key: {other}"),
    }
    publish_snapshot().await;
}

/// "Reset to defaults" — restores Audio + Playback defaults
/// (settings.rs handle_reset: store resets + apply + snapshot).
pub async fn settings_reset(runtime: &Arc<AppRuntime<LoggingAdapter>>) {
    let Some(_owner_action) = begin_audio_owner_action("reset live audio settings", false) else {
        return;
    };
    if let Err(e) = with_audio(|s| s.reset_all().map(|_| ())) {
        log::error!("[qbz-qt] audio settings reset failed: {e}");
    }
    if let Err(e) = with_playback(|s| s.reset_all().map(|_| ())) {
        log::error!("[qbz-qt] playback preferences reset failed: {e}");
    }
    // Streaming Quality is deliberately NOT reset. It is a UI-only pref that
    // belongs to neither domain store, and the reference says so out loud
    // (`crates/qbz/src/settings.rs:1333-1336`: "intentionally left
    // untouched"). This port used to force it back to `hires_plus`, so a
    // user on a metered connection who reset their AUDIO settings silently
    // got hi-res streaming again.
    //
    // #638 fix 3, trigger 5 — the reset turns "Limit quality to device" off,
    // so this refresh is the one that CLEARS the cap (and, through the
    // before/after comparison, drops the tier-keyed cache the old cap filled).
    refresh_device_cap(runtime).await;
    apply_audio_with_owner(runtime, Apply::Reinit);
    publish_snapshot().await;
}

/// The refresh/release button next to the output device (settings.rs:
/// frees a held ALSA-exclusive device and re-enumerates).
pub async fn refresh_devices(runtime: &Arc<AppRuntime<LoggingAdapter>>) {
    let Some(_owner_action) = begin_audio_owner_action("refresh output devices", false) else {
        return;
    };
    // Release whatever the player holds, then re-enumerate from scratch (the
    // whole point of the button is a device that was not in the last list).
    if let Err(error) = release_output_device_with_owner(runtime).await {
        report_release_failure(&error);
    }
    invalidate_device_cache();
    // #638 fix 3, trigger 6 — Qt-only, and the easiest of the six to miss.
    // This button exists precisely for hotplug: the hardware behind the
    // selection can change without the SELECTION changing, which is the case
    // a device-change trigger cannot see. It matters most under "System
    // default". Placed after the re-enumeration so the probe reads the new
    // device list, and before the publish so the row lands settled.
    refresh_device_cap(runtime).await;
    reconcile_alsa_hardware_volume_with_owner(runtime).await;
    publish_snapshot().await;
}

#[cfg(test)]
mod local_tab_order_tests {
    use super::*;

    fn mixer_candidate() -> HardwareVolumeCandidate {
        HardwareVolumeCandidate {
            id: AlsaMixerControlId {
                name: "D50 III".to_string(),
                index: 2,
            },
            label: "D50 III,2".to_string(),
            channels: vec!["front_left".to_string(), "front_right".to_string()],
            raw_range: Some(qbz_audio::alsa_hardware_volume::HardwareVolumeRange {
                min: 0,
                max: 255,
            }),
            db_range: Some(qbz_audio::alsa_hardware_volume::HardwareVolumeRange {
                min: -12_700,
                max: 0,
            }),
            current_values: Vec::new(),
            current_volume: Some(0.5),
            has_playback_switch: true,
            recommended: true,
            writability: qbz_audio::alsa_hardware_volume::HardwareVolumeWritability::Verified,
            rejection_reason: None,
        }
    }

    #[test]
    fn absent_or_invalid_order_uses_the_complete_default() {
        let expected: Vec<String> = LOCAL_TAB_DEFAULT_ORDER
            .iter()
            .map(|id| (*id).to_string())
            .collect();
        assert_eq!(normalize_local_tab_order(None), expected);
        assert_eq!(
            normalize_local_tab_order(Some(&serde_json::json!("albums"))),
            expected
        );
    }

    #[test]
    fn changing_interface_scale_preserves_the_native_window_footprint() {
        let mut doc = serde_json::json!({
            "ui_scale": "small",
            "window_width": 1200.0,
            "window_height": 700.0
        })
        .as_object()
        .unwrap()
        .clone();
        let native_before = (
            doc["window_width"].as_f64().unwrap() * ui_scale_factor_for("small") as f64,
            doc["window_height"].as_f64().unwrap() * ui_scale_factor_for("small") as f64,
        );

        assert!(set_ui_scale_in_doc(&mut doc, "xl"));

        let native_after = (
            doc["window_width"].as_f64().unwrap() * ui_scale_factor_for("xl") as f64,
            doc["window_height"].as_f64().unwrap() * ui_scale_factor_for("xl") as f64,
        );
        assert!((native_before.0 - native_after.0).abs() < 0.001);
        assert!((native_before.1 - native_after.1).abs() < 0.001);
        assert_eq!(doc["ui_scale"].as_str(), Some("xl"));
        assert!(!set_ui_scale_in_doc(&mut doc, "xl"));
    }

    #[test]
    fn stored_order_deduplicates_filters_and_appends_missing_tabs() {
        let value = serde_json::json!(["tracks", "future-tab", "tracks", "albums", 7, "artists"]);
        assert_eq!(
            normalize_local_tab_order(Some(&value)),
            ["tracks", "albums", "artists", "genres", "folders"]
        );
    }

    #[test]
    fn kiosk_fallback_contract_always_has_a_supported_tab() {
        let normalized = normalize_local_tab_order(Some(&serde_json::json!([
            "genres", "folders", "albums", "artists", "tracks"
        ])));
        assert_eq!(local_landing_tab_from(&normalized, false), "genres");
        assert_eq!(local_landing_tab_from(&normalized, true), "albums");
        let tracks_first = vec!["tracks".to_string(), "albums".to_string()];
        assert_eq!(local_landing_tab_from(&tracks_first, true), "albums");
        assert_eq!(local_landing_tab_from(&tracks_first, false), "tracks");
    }

    #[test]
    fn genre_filter_position_is_closed_and_defaults_to_top() {
        for value in LOCAL_GENRE_FILTER_POSITION_VALUES {
            assert_eq!(local_genre_filters_position_from(value), *value);
        }
        assert_eq!(local_genre_filters_position_from("future-edge"), "top");
        assert_eq!(local_genre_filters_position_from(""), "top");
    }

    #[test]
    fn software_tier_hides_native_scope_modes_without_touching_legacy_modes() {
        for mode in 0..=2 {
            assert_eq!(large_spectrum_mode_for_tier(mode, false), mode);
        }
        assert_eq!(large_spectrum_mode_for_tier(3, false), 0);
        assert_eq!(large_spectrum_mode_for_tier(4, false), 0);
        assert_eq!(large_spectrum_mode_for_tier(3, true), 3);
        assert_eq!(large_spectrum_mode_for_tier(4, true), 4);
    }

    #[test]
    fn hardware_volume_statuses_are_typed_and_user_visible() {
        let statuses = [
            HardwareVolumeUiStatus::Probing,
            HardwareVolumeUiStatus::Ambiguous,
            HardwareVolumeUiStatus::Stale,
            HardwareVolumeUiStatus::PermissionDenied,
            HardwareVolumeUiStatus::DeviceBusy,
            HardwareVolumeUiStatus::DeviceUnavailable,
            HardwareVolumeUiStatus::NoMixer,
            HardwareVolumeUiStatus::Unsupported,
            HardwareVolumeUiStatus::WriteFailed,
        ];
        let mut keys = std::collections::HashSet::new();
        for status in statuses {
            assert!(keys.insert(status.key()), "duplicate status key");
            assert!(!status.message().is_empty(), "missing status message");
        }
    }

    #[test]
    fn hardware_volume_option_exposes_exact_identity_range_and_channels() {
        let candidate = mixer_candidate();
        assert_eq!(candidate.id.to_string(), "D50 III,2");
        let detail = hardware_volume_candidate_detail(&candidate);
        assert!(detail.contains("-127.00–0.00 dB"), "{detail}");
        assert!(detail.contains("front_left"), "{detail}");
        assert!(detail.starts_with('★'), "{detail}");
    }

    #[test]
    fn qml_uses_the_shared_closed_select_for_mixer_identity() {
        let audio_qml = include_str!("../qml/settings/AudioSettings.qml");
        let select_qml = include_str!("../qml/controls/QbzSelect.qml");
        assert!(audio_qml.contains("alsaHardwareVolumeOptions"));
        assert!(audio_qml.contains("QbzSelect"));
        assert!(!audio_qml.contains("TextInput"));
        assert!(select_qml.contains("function openPopup"));
        assert!(select_qml.contains("function optDetail"));
        assert!(select_qml.contains("selectRoot.optDetail(optRow.index)"));
    }

    #[test]
    fn only_alsa_direct_without_a_hardware_mixer_requires_unity_volume() {
        let mut audio = qbz_audio::settings::AudioSettings::default();
        assert!(!requires_alsa_direct_unity(&audio));

        audio.backend_type = Some(AudioBackendType::Alsa);
        audio.output_device = Some("front:CARD=USB,DEV=0".to_string());
        audio.alsa_plugin = Some(AlsaPlugin::Hw);
        assert_eq!(
            requires_alsa_direct_unity(&audio),
            cfg!(target_os = "linux")
        );

        audio.alsa_hardware_volume = true;
        assert!(!requires_alsa_direct_unity(&audio));
        audio.alsa_hardware_volume = false;

        audio.alsa_plugin = Some(AlsaPlugin::PlugHw);
        assert_eq!(
            requires_alsa_direct_unity(&audio),
            cfg!(target_os = "linux")
        );
        audio.alsa_plugin = Some(AlsaPlugin::Pcm);
        assert!(!requires_alsa_direct_unity(&audio));

        audio.alsa_plugin = Some(AlsaPlugin::Hw);
        audio.output_device = None;
        assert!(!requires_alsa_direct_unity(&audio));
        audio.backend_type = Some(AudioBackendType::PipeWire);
        audio.output_device = Some("front:CARD=USB,DEV=0".to_string());
        assert!(!requires_alsa_direct_unity(&audio));
    }
}

#[cfg(test)]
mod wallpaper_blur_tests {
    use super::*;

    #[test]
    fn the_slider_percent_maps_onto_qts_blur_range() {
        assert_eq!(wallpaper_blur_from_percent(0), 0.0);
        assert_eq!(wallpaper_blur_from_percent(75), 0.75);
        assert_eq!(wallpaper_blur_from_percent(100), 1.0);
        assert_eq!(wallpaper_blur_from_percent(-20), 0.0);
        assert_eq!(wallpaper_blur_from_percent(250), 1.0);
    }

    #[test]
    fn a_hand_edited_value_is_held_to_qts_range() {
        assert_eq!(clamp_wallpaper_blur(1.7), 1.0);
        assert_eq!(clamp_wallpaper_blur(-0.2), 0.0);
        assert_eq!(clamp_wallpaper_blur(0.4), 0.4);
        assert_eq!(clamp_wallpaper_blur(f32::NAN), WALLPAPER_BLUR_DEFAULT);
    }
}

#[cfg(test)]
mod theme_fieldset_doc_tests {
    use super::*;

    #[test]
    fn the_settings_document_names_the_fieldset_states_for_qml() {
        let doc = SettingsDoc {
            theme_custom_collapsed: true,
            ambient_options_collapsed: true,
            ..Default::default()
        };
        let json = serde_json::to_value(&doc).expect("settings document serializes");
        assert_eq!(json["themeCustomCollapsed"], true);
        assert_eq!(json["themeAutoCollapsed"], false);
        assert_eq!(json["ambientOptionsCollapsed"], true);
    }
}

#[cfg(test)]
mod exclusive_gate_tests {
    use super::*;

    #[test]
    fn memory_and_appearance_documents_use_the_qml_field_names() {
        let doc = SettingsDoc {
            playback_memory_profile: "low".into(),
            appearance_profile: "custom".into(),
            playback_memory_apply_busy: true,
            playback_storage: playback_storage::Snapshot {
                candidate: "/mnt/music".into(),
                ..Default::default()
            },
            ..Default::default()
        };
        let json = serde_json::to_value(doc).unwrap();
        assert_eq!(json["playbackMemoryProfile"], "low");
        assert_eq!(json["appearanceProfile"], "custom");
        assert_eq!(json["playbackMemoryApplyBusy"], true);
        assert_eq!(json["playbackStorage"]["candidate"], "/mnt/music");
    }

    #[test]
    fn settings_doc_publishes_backend_is_coreaudio_under_the_name_qml_reads() {
        // AudioSettings.qml gates the Exclusive-mode row on
        // `root.doc.backendIsCoreAudio === true`; the serde rename is the
        // contract between the two files.
        let doc = SettingsDoc {
            backend_is_coreaudio: true,
            ..Default::default()
        };
        let json = serde_json::to_value(&doc).expect("SettingsDoc serialises");
        assert_eq!(json["backendIsCoreAudio"], serde_json::Value::Bool(true));
    }

    #[test]
    fn backend_switch_clears_exclusive_unless_the_backend_honours_it() {
        assert!(!backend_switch_clears_exclusive(AudioBackendType::Alsa));
        assert!(backend_switch_clears_exclusive(AudioBackendType::PipeWire));
        // #748: System default is CoreAudio exclusive on macOS and inert elsewhere.
        assert_eq!(
            backend_switch_clears_exclusive(AudioBackendType::SystemDefault),
            !cfg!(target_os = "macos")
        );
    }
}
