// crates/qbz-app/src/settings/bundle.rs — the settings portability engine
// (04-settings-portability.md). ONE module, shared by `qbzd` (P0 CLI) and the
// desktop Settings modal (P0/OD7, plan T18): `export(source, opts) -> Bundle`,
// `plan(bundle, target, opts, live) -> ImportPlan`, `apply(plan, target, uid)
// -> ImportReport`.
//
// The load-bearing invariant (04 §1): CLASSIFICATION LIVES IN THE IMPORTER,
// NEVER IN THE BUNDLE. A bundle is data with zero authority over how it is
// applied — the §3 table (below, `classify_audio_key` + the domain loops) is
// versioned with THIS code and applied to whatever is present in the file,
// including fields a well-behaved exporter never writes (a hand-added
// `volume` is skipped no matter how it got there — §1 corollary).
//
// TDD: the inline `#[cfg(test)]` module IS the normative test suite — one test
// per §3/§5 rule. The engine takes a `LiveSystem` by injection so those tests
// never need real audio hardware.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use qbz_audio::settings::{AudioSettings, AudioSettingsStore};
use qbz_audio::AudioBackendType;

use crate::settings::daemon_prefs;
use crate::settings::playback::{PlaybackPreferences, PlaybackPreferencesStore};
use crate::settings::scrobblers::ScrobblerSettingsStore;

/// The bundle schema version this importer implements and this exporter writes
/// (04 §1). Hard-gated on import (`plan` step 2, §5.6). v1 is the floor.
pub const SCHEMA_VERSION: i64 = 1;

// ============================ public types ============================

/// One versioned JSON document (04 §1). The header fields are typed; every
/// settings domain rides in `domains` as raw JSON so the importer can classify
/// whatever is present (§1 corollary). Serializes FLAT — the domains sit at the
/// top level alongside the header, exactly like the §2.9 example.
#[derive(Debug, Clone, Serialize)]
pub struct Bundle {
    pub schema_version: i64,
    /// RFC 3339 UTC timestamp of export.
    pub created_at: String,
    pub source: BundleSource,
    #[serde(flatten)]
    pub domains: Map<String, Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BundleSource {
    pub app_version: String,
    /// `"desktop"` | `"daemon"` — which profile was read.
    pub profile: String,
    pub hostname: String,
}

/// A profile's config + data roots. For the daemon these are the daemon roots
/// (`~/.config/qbzd`, `~/.local/share/qbzd`); for the desktop the global roots
/// (`~/.config/qbz`, `~/.local/share/qbz`).
#[derive(Debug, Clone)]
pub struct ProfilePaths {
    pub config_root: PathBuf,
    pub data_root: PathBuf,
}

/// Where an export reads its settings from (04 §4.1).
pub enum ExportSource {
    /// The GLOBAL desktop stores at `~/.local/share/qbz` (read-only; the ONLY
    /// place desktop paths are legal — the per-user `users/<uid>/` copies are
    /// Tauri-era ghosts, never read).
    Desktop,
    /// The daemon's own profile roots.
    Daemon(ProfilePaths),
}

#[derive(Debug, Clone, Default)]
pub struct ExportOptions {
    pub include_auth: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ImportOptions {
    pub include_auth: bool,
    pub trust_dsd: bool,
    /// Repeatable `--remap OLD=NEW` prefix rewrites for `library_folders`.
    pub remap: Vec<(String, String)>,
    /// True when there is no interactive terminal — machine fields that would
    /// need a prompt fall to safe defaults instead of hanging (§5.3 step 4).
    pub non_tty: bool,
}

/// Injected snapshot of the local audio system so classification is testable
/// without hardware (`BackendManager::available_backends()` + the chosen
/// backend's device enumeration).
#[derive(Debug, Clone, Default)]
pub struct LiveSystem {
    pub backends: Vec<String>,
    /// `(id, label)` pairs for the chosen backend.
    pub devices: Vec<(String, String)>,
}

/// One line of the three-bucket summary (§5.4). `old` is set only for *adapted*
/// lines (which render `old -> new`); *skipped* lines carry the reason in `why`
/// and leave `new` empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanLine {
    pub key: String,
    pub old: Option<String>,
    pub new: String,
    pub why: String,
}

/// A machine device that needs an interactive re-pick (§5.3 step 4, TTY path).
#[derive(Debug, Clone)]
pub struct DevicePick {
    pub wanted: String,
    /// The backend the options were enumerated for — the §5.4 prompt names it
    /// ("Available on Alsa:").
    pub backend: String,
    pub options: Vec<(String, String)>,
}

/// The operator's answer to a [`DevicePick`] (from the CLI/TUI). Fed back into
/// [`replan_with_device`].
#[derive(Debug, Clone)]
pub enum DeviceChoice {
    SystemDefault,
    Device { id: String, label: String },
}

/// The classified plan (`plan` output): the three display buckets, plus the
/// execution list (`writes`), an optional device re-pick, and the decrypted
/// auth token to validate before any write.
#[derive(Debug, Clone, Default)]
pub struct ImportPlan {
    pub applied: Vec<PlanLine>,
    pub adapted: Vec<PlanLine>,
    pub skipped: Vec<PlanLine>,
    pub device_pick: Option<DevicePick>,
    /// Present only when `--include-auth` AND the bundle carries `auth`. The CLI
    /// validates it via `validate_token` BEFORE calling `apply` (§5.3 step 5).
    pub auth_token: Option<String>,
    /// Cross-check uid from the bundle (`auth.user_id`); the authoritative uid
    /// is the validated login's (§5.7).
    pub bundle_user_id: Option<u64>,
    /// The typed write actions backing `applied` + `adapted` (display strings
    /// are lossy — e.g. `(auto)` — so execution rides raw JSON values here).
    pub writes: Vec<(String, Value)>,
    /// True when a routing-critical field (backend/device/exclusive) changed —
    /// the CLI's reload line owns the honesty note (§5.3 step 7).
    pub routing_critical_changed: bool,
}

/// The outcome of `apply` (§5.3 step 8): bucket counts + per-domain results so
/// a mid-apply I/O failure is reported honestly.
#[derive(Debug, Clone, Default)]
pub struct ImportReport {
    pub applied: usize,
    pub adapted: usize,
    pub skipped: usize,
    pub per_domain: Vec<(String, Result<(), String>)>,
}

/// Everything that can go wrong in the engine. `Display` renders a plain
/// message; the CLI maps the daemon-facing variants to the verbatim 04 copies
/// in `cli::copy`.
#[derive(Debug)]
pub enum BundleError {
    /// Step 1: unreadable file / invalid JSON.
    Parse(String),
    /// Step 2: `schema_version` missing or non-integer.
    VersionMalformed,
    /// Step 2: bundle newer than this importer (§5.6).
    VersionTooNew { bundle: i64, supported: i64 },
    /// Export: no desktop profile found (§4.1).
    NoDesktopProfile,
    /// Export: `--include-auth` but the desktop token would not decrypt (IV1,
    /// §4.1 — portal-secret bound to the desktop session).
    TokenDecryptFailed,
    /// Any store/file I/O failure (export write or apply write).
    Io(String),
}

impl std::fmt::Display for BundleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BundleError::Parse(m) => write!(f, "cannot read bundle: {m}"),
            BundleError::VersionMalformed => {
                write!(f, "bundle has no valid integer schema_version")
            }
            BundleError::VersionTooNew { bundle, supported } => write!(
                f,
                "this bundle is schema v{bundle}; this qbzd understands up to v{supported}"
            ),
            BundleError::NoDesktopProfile => write!(f, "no desktop profile found"),
            BundleError::TokenDecryptFailed => {
                write!(f, "could not decrypt the desktop Qobuz token")
            }
            BundleError::Io(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for BundleError {}

// ============================ Bundle (de)serialization ============================

impl Bundle {
    /// Serialize to pretty JSON (the on-disk `.qbzb` form — plain JSON).
    pub fn to_json_string(&self) -> Result<String, BundleError> {
        serde_json::to_string_pretty(self).map_err(|e| BundleError::Io(e.to_string()))
    }

    /// True when the document actually CARRIES a secret value: an `auth` token,
    /// or a non-blank scrobbler secret. The export-side §3 warning keys on this
    /// — not on the auth domain alone (a bundle whose only secrets are scrobbler
    /// tokens still needs the warning).
    pub fn contains_secrets(&self) -> bool {
        let auth_token = self
            .domains
            .get("auth")
            .and_then(Value::as_object)
            .and_then(|a| a.get("user_auth_token"))
            .and_then(Value::as_str)
            .map(|t| !t.is_empty())
            .unwrap_or(false);
        if auth_token {
            return true;
        }
        self.domains
            .get("integrations")
            .and_then(|i| i.get("scrobblers"))
            .and_then(Value::as_object)
            .map(|s| {
                ["lastfm_session_key", "listenbrainz_token"].iter().any(|k| {
                    s.get(*k)
                        .and_then(Value::as_str)
                        .map(|v| !v.is_empty())
                        .unwrap_or(false)
                })
            })
            .unwrap_or(false)
    }

    /// Parse a bundle from JSON text (import step 1). The version gate (step 2)
    /// runs in [`plan`]; here we only require a JSON object with an integer
    /// `schema_version`, so a malformed version is caught early.
    pub fn parse(text: &str) -> Result<Bundle, BundleError> {
        let value: Value =
            serde_json::from_str(text).map_err(|e| BundleError::Parse(e.to_string()))?;
        let mut obj = match value {
            Value::Object(m) => m,
            _ => return Err(BundleError::Parse("bundle root is not a JSON object".into())),
        };
        let schema_version = match obj.remove("schema_version") {
            Some(Value::Number(n)) if n.is_i64() || n.is_u64() => {
                n.as_i64().unwrap_or(i64::MAX)
            }
            _ => return Err(BundleError::VersionMalformed),
        };
        let created_at = obj
            .remove("created_at")
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        let source = obj
            .remove("source")
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();
        Ok(Bundle {
            schema_version,
            created_at,
            source,
            domains: obj,
        })
    }
}

// ============================ export (§4.1) ============================

/// Read a profile's settings into a [`Bundle`]. `--from desktop` reads the
/// GLOBAL desktop stores (the ONLY legal desktop-path access); `--from daemon`
/// reads the daemon roots. Domains the source cannot provide are ABSENT, never
/// empty objects (§2.9).
pub fn export(source: ExportSource, opts: &ExportOptions) -> Result<Bundle, BundleError> {
    let (paths, profile) = match &source {
        ExportSource::Desktop => (desktop_paths(), "desktop"),
        ExportSource::Daemon(p) => (
            ProfilePaths {
                config_root: p.config_root.clone(),
                data_root: p.data_root.clone(),
            },
            "daemon",
        ),
    };

    if matches!(source, ExportSource::Desktop) && !paths.data_root.exists() {
        return Err(BundleError::NoDesktopProfile);
    }

    let mut domains: Map<String, Value> = Map::new();

    // playback — PlaybackPreferences (global playback_preferences.db)
    if let Some(prefs) = read_playback_prefs(&paths.data_root) {
        domains.insert("playback".into(), playback_to_json(&prefs));
    }

    // audio — full-struct serde of AudioSettings (importer owns classification)
    if let Some(audio) = read_audio_settings(&paths.data_root) {
        if let Ok(v) = serde_json::to_value(&audio) {
            domains.insert("audio".into(), v);
        }
    }

    // prefs.streaming_quality — daemon_prefs (daemon) / ui_prefs.json (desktop)
    let streaming_quality = match &source {
        ExportSource::Daemon(_) => Some(daemon_prefs::load_at(&paths.data_root).streaming_quality),
        ExportSource::Desktop => read_ui_prefs_streaming_quality(&paths.data_root),
    };
    if let Some(q) = streaming_quality {
        let mut prefs = Map::new();
        prefs.insert("streaming_quality".into(), Value::String(q));
        domains.insert("prefs".into(), Value::Object(prefs));
    }

    // qconnect — device_name + startup_mode (device_uuid is NEVER exported, §2.4)
    if let Some(qc) = read_qconnect_domain(&paths.data_root) {
        domains.insert("qconnect".into(), qc);
    }

    // per-user domains — resolve the source uid.
    let uid = match &source {
        ExportSource::Desktop => crate::user_data::UserDataPaths::load_last_user_id(),
        ExportSource::Daemon(_) => read_last_user_id(&paths.data_root),
    };
    match uid {
        Some(uid) => {
            if let Some(scrob) = read_scrobblers(&paths.data_root, uid, opts.include_auth) {
                let mut integrations = Map::new();
                integrations.insert("scrobblers".into(), scrob);
                domains.insert("integrations".into(), Value::Object(integrations));
            }
            if let Some(folders) = read_library_folders(&paths.data_root) {
                domains.insert("library_folders".into(), folders);
            }
        }
        None => {
            log::info!(
                "[bundle] no last_user_id under this profile — per-user domains \
                 (integrations, library_folders) omitted"
            );
        }
    }

    // auth — SECRET, opt-in (§2.7). Export-side half of the double gate.
    if opts.include_auth {
        let token = load_decrypted_token(&source, &paths)?;
        if let Some(token) = token {
            let mut auth = Map::new();
            auth.insert("user_auth_token".into(), Value::String(token));
            if let Some(uid) = uid {
                auth.insert("user_id".into(), Value::Number(uid.into()));
            }
            domains.insert("auth".into(), Value::Object(auth));
        }
    }

    Ok(Bundle {
        schema_version: SCHEMA_VERSION,
        created_at: now_rfc3339(),
        source: BundleSource {
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            profile: profile.to_string(),
            hostname: hostname(),
        },
        domains,
    })
}

// ============================ plan (§5.3 steps 2–4) ============================

/// Classify every present field against the local system, producing the three
/// display buckets + the typed write list. Steps 2–4 of §5.3 (read+parse is the
/// caller's `Bundle::parse`, step 1). Non-interactive: when a device needs a
/// pick, TTY callers get `device_pick` set and should call
/// [`replan_with_device`] after prompting.
pub fn plan(
    bundle: &Bundle,
    target: &ProfilePaths,
    opts: &ImportOptions,
    live: &LiveSystem,
) -> Result<ImportPlan, BundleError> {
    build_plan(bundle, target, opts, live, None)
}

/// Re-run [`plan`] with the operator's device choice resolved (TTY re-pick,
/// §5.4). The returned plan has `device_pick == None`.
pub fn replan_with_device(
    bundle: &Bundle,
    target: &ProfilePaths,
    opts: &ImportOptions,
    live: &LiveSystem,
    chosen: DeviceChoice,
) -> Result<ImportPlan, BundleError> {
    build_plan(bundle, target, opts, live, Some(chosen))
}

fn build_plan(
    bundle: &Bundle,
    target: &ProfilePaths,
    opts: &ImportOptions,
    live: &LiveSystem,
    forced_device: Option<DeviceChoice>,
) -> Result<ImportPlan, BundleError> {
    // Step 2: version gate.
    if bundle.schema_version < 1 {
        return Err(BundleError::VersionMalformed);
    }
    if bundle.schema_version > SCHEMA_VERSION {
        return Err(BundleError::VersionTooNew {
            bundle: bundle.schema_version,
            supported: SCHEMA_VERSION,
        });
    }

    let mut plan = ImportPlan::default();

    // Does a uid exist (or will one after auth validation)? Drives whether
    // per-user domains apply (§5.7).
    let auth_present = bundle
        .domains
        .get("auth")
        .and_then(Value::as_object)
        .map(|a| a.contains_key("user_auth_token"))
        .unwrap_or(false);
    let uid_will_exist =
        (opts.include_auth && auth_present) || read_last_user_id(&target.data_root).is_some();

    // Step 3+4: classify each present domain.
    for (domain, value) in &bundle.domains {
        match domain.as_str() {
            "playback" => plan_playback(value, &mut plan),
            "audio" => plan_audio(value, target, opts, live, &forced_device, &mut plan),
            "prefs" => plan_prefs(value, &mut plan),
            "qconnect" => plan_qconnect(value, target, &mut plan),
            "integrations" => plan_integrations(value, opts, uid_will_exist, &mut plan),
            "library_folders" => plan_library_folders(value, &mut plan),
            "auth" => plan_auth(value, opts, &mut plan),
            // §1 corollary: a top-level `volume` domain is NEVER-class, always.
            v if v.eq_ignore_ascii_case("volume") => {
                plan.skipped.push(skip_line(domain, VOLUME_SKIP_WHY));
            }
            _ => {
                plan.skipped.push(skip_line(domain, UNKNOWN_WHY));
            }
        }
    }

    Ok(plan)
}

// ============================ apply (§5.3 step 6) ============================

/// Execute a plan against the daemon-root stores. Reached only after every
/// check passed (validate-all-then-apply). Pure setter writes — re-running is
/// safe and idempotent (§5.3 step 6). `validated_uid` is the authoritative uid
/// from the validated login (§5.7); when absent, the daemon's own
/// `last_user_id` is consulted for per-user writes.
pub fn apply(
    plan: &ImportPlan,
    target: &ProfilePaths,
    validated_uid: Option<u64>,
) -> Result<ImportReport, BundleError> {
    let mut report = ImportReport {
        applied: plan.applied.len(),
        adapted: plan.adapted.len(),
        skipped: plan.skipped.len(),
        per_domain: Vec::new(),
    };

    let uid = validated_uid.or_else(|| read_last_user_id(&target.data_root));

    // auth first: persist token + last_user_id + ensure users/<uid>/ (§5.7).
    if let (Some(token), Some(uid)) = (&plan.auth_token, validated_uid) {
        match persist_auth(target, token, uid) {
            Ok(()) => report.per_domain.push(("auth".into(), Ok(()))),
            Err(e) => {
                report.per_domain.push(("auth".into(), Err(e.clone())));
                return Err(BundleError::Io(e));
            }
        }
    }

    // group writes by store so each store opens once.
    let mut audio_writes: Vec<(&str, &Value)> = Vec::new();
    let mut playback_writes: Vec<(&str, &Value)> = Vec::new();
    let mut prefs_quality: Option<&Value> = None;
    let mut qconnect_writes: Vec<(&str, &Value)> = Vec::new();
    let mut scrobbler_writes: Vec<(&str, &Value)> = Vec::new();

    for (key, value) in &plan.writes {
        if let Some(rest) = key.strip_prefix("audio.") {
            audio_writes.push((rest, value));
        } else if let Some(rest) = key.strip_prefix("playback.") {
            playback_writes.push((rest, value));
        } else if key == "prefs.streaming_quality" {
            prefs_quality = Some(value);
        } else if let Some(rest) = key.strip_prefix("qconnect.") {
            qconnect_writes.push((rest, value));
        } else if let Some(rest) = key.strip_prefix("integrations.scrobblers.") {
            scrobbler_writes.push((rest, value));
        }
    }

    if !audio_writes.is_empty() {
        let r = apply_audio_writes(&target.data_root, &audio_writes);
        let failed = r.is_err();
        report.per_domain.push(("audio".into(), r.clone()));
        if failed {
            return Err(BundleError::Io(r.unwrap_err()));
        }
    }
    if !playback_writes.is_empty() {
        let r = apply_playback_writes(&target.data_root, &playback_writes);
        let failed = r.is_err();
        report.per_domain.push(("playback".into(), r.clone()));
        if failed {
            return Err(BundleError::Io(r.unwrap_err()));
        }
    }
    if let Some(q) = prefs_quality {
        let r = apply_prefs_quality(&target.data_root, q);
        let failed = r.is_err();
        report.per_domain.push(("prefs".into(), r.clone()));
        if failed {
            return Err(BundleError::Io(r.unwrap_err()));
        }
    }
    if !qconnect_writes.is_empty() {
        let r = apply_qconnect_writes(&target.data_root, &qconnect_writes);
        let failed = r.is_err();
        report.per_domain.push(("qconnect".into(), r.clone()));
        if failed {
            return Err(BundleError::Io(r.unwrap_err()));
        }
    }
    if !scrobbler_writes.is_empty() {
        match uid {
            Some(uid) => {
                let r = apply_scrobbler_writes(&target.data_root, uid, &scrobbler_writes);
                let failed = r.is_err();
                report.per_domain.push(("integrations".into(), r.clone()));
                if failed {
                    return Err(BundleError::Io(r.unwrap_err()));
                }
            }
            None => report.per_domain.push((
                "integrations".into(),
                Err("no user on this daemon — skipped".into()),
            )),
        }
    }

    Ok(report)
}

// ============================ classification helpers ============================

const UNKNOWN_WHY: &str = "unknown field (bundle from a newer QBZ?)";
const VOLUME_SKIP_WHY: &str = "never imported (volume hazard — a daemon may drive a power amp)";
const CACHE_SKIP_WHY: &str = "never imported (source-machine device cache)";

fn skip_line(key: &str, why: &str) -> PlanLine {
    PlanLine {
        key: key.to_string(),
        old: None,
        new: String::new(),
        why: why.to_string(),
    }
}

fn applied_line(plan: &mut ImportPlan, key: &str, value: &Value, why: &str) {
    plan.applied.push(PlanLine {
        key: key.to_string(),
        old: None,
        new: render_value(value),
        why: why.to_string(),
    });
    plan.writes.push((key.to_string(), value.clone()));
}

/// SECRET-class applied line (§5.4): the VALUE COLUMN IS MASKED — the real
/// value goes only into the write list, never into the rendered summary
/// (terminal scrollback and CI logs are not a place for bearer tokens).
/// Non-empty → `(secret, applied)`; empty → `(empty)`, matching the §5.4 example.
fn applied_secret_line(plan: &mut ImportPlan, key: &str, value: &Value) {
    let masked = match value.as_str() {
        Some(s) if !s.is_empty() => "(secret, applied)",
        _ => "(empty)",
    };
    plan.applied.push(PlanLine {
        key: key.to_string(),
        old: None,
        new: masked.to_string(),
        why: String::new(),
    });
    plan.writes.push((key.to_string(), value.clone()));
}

fn adapted_line(plan: &mut ImportPlan, key: &str, old: &Value, new: &Value, why: &str) {
    plan.adapted.push(PlanLine {
        key: key.to_string(),
        old: Some(render_value(old)),
        new: render_value(new),
        why: why.to_string(),
    });
    plan.writes.push((key.to_string(), new.clone()));
}

/// Render a JSON value for the human summary (§5.4): null → `(auto)`, empty
/// string → `(empty)`, bools/numbers/strings verbatim.
fn render_value(v: &Value) -> String {
    match v {
        Value::Null => "(auto)".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) if s.is_empty() => "(empty)".to_string(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

// ---- playback — all PORTABLE, applied verbatim (§2.1) ----
const PLAYBACK_KEYS: &[&str] = &[
    "autoplay_mode",
    "show_context_icon",
    "persist_session",
    "resume_playback_position",
];

fn plan_playback(value: &Value, plan: &mut ImportPlan) {
    let Some(map) = value.as_object() else {
        return;
    };
    for (k, v) in map {
        if k.eq_ignore_ascii_case("volume") {
            plan.skipped.push(skip_line(&format!("playback.{k}"), VOLUME_SKIP_WHY));
        } else if PLAYBACK_KEYS.contains(&k.as_str()) {
            applied_line(plan, &format!("playback.{k}"), v, "");
        } else {
            plan.skipped.push(skip_line(&format!("playback.{k}"), UNKNOWN_WHY));
        }
    }
}

// ---- prefs — streaming_quality PORTABLE; language/volume out of v1 (§2.3) ----
fn plan_prefs(value: &Value, plan: &mut ImportPlan) {
    let Some(map) = value.as_object() else {
        return;
    };
    for (k, v) in map {
        match k.as_str() {
            "streaming_quality" => applied_line(plan, "prefs.streaming_quality", v, ""),
            _ if k.eq_ignore_ascii_case("volume") => {
                plan.skipped.push(skip_line(&format!("prefs.{k}"), VOLUME_SKIP_WHY));
            }
            "language" => plan.skipped.push(skip_line(
                "prefs.language",
                "not imported (no TUI i18n in daemon v1)",
            )),
            _ => plan.skipped.push(skip_line(&format!("prefs.{k}"), UNKNOWN_WHY)),
        }
    }
}

// ---- audio — the interdependent machine block (§2.2, §3, §5.3 step 4) ----
const AUDIO_PORTABLE: &[&str] = &[
    "stream_first_track",
    "stream_buffer_seconds",
    "streaming_only",
    "playback_cache",
    "normalization_enabled",
    "normalization_target_lufs",
    "normalization_prevent_clipping",
    "gapless_enabled",
    "allow_quality_fallback",
    "sync_audio_on_startup",
    "limit_quality_to_device",
    "preferred_sample_rate",
];
const AUDIO_INTENT_FLAGS: &[&str] = &[
    "exclusive_mode",
    "dac_passthrough",
    "pw_force_bitperfect",
    "skip_sink_switch",
    "reserve_dac_while_running",
];
const AUDIO_NEVER_CACHES: &[&str] = &["device_max_sample_rate", "device_sample_rate_limits"];

fn plan_audio(
    value: &Value,
    target: &ProfilePaths,
    opts: &ImportOptions,
    live: &LiveSystem,
    forced_device: &Option<DeviceChoice>,
    plan: &mut ImportPlan,
) {
    let Some(map) = value.as_object() else {
        return;
    };
    let current = read_audio_settings(&target.data_root).unwrap_or_default();

    // First pass: the simple classifications.
    for (k, v) in map {
        if k.eq_ignore_ascii_case("volume") {
            plan.skipped.push(skip_line(&format!("audio.{k}"), VOLUME_SKIP_WHY));
        } else if AUDIO_NEVER_CACHES.contains(&k.as_str()) {
            plan.skipped.push(skip_line(&format!("audio.{k}"), CACHE_SKIP_WHY));
        } else if AUDIO_PORTABLE.contains(&k.as_str()) {
            applied_line(plan, &format!("audio.{k}"), v, "");
        } else if k == "quality_fallback_behavior" {
            plan_quality_fallback(v, plan);
        }
    }

    // Second pass: the machine block (backend + device + intent + alsa + dsd).
    plan_audio_machine(map, &current, opts, live, forced_device, plan);

    // Any genuinely-unknown audio keys.
    for (k, _) in map {
        let known = AUDIO_PORTABLE.contains(&k.as_str())
            || AUDIO_INTENT_FLAGS.contains(&k.as_str())
            || AUDIO_NEVER_CACHES.contains(&k.as_str())
            || matches!(
                k.as_str(),
                "quality_fallback_behavior"
                    | "backend_type"
                    | "output_device"
                    | "alsa_plugin"
                    | "alsa_hardware_volume"
                    | "alsa_hardware_volume_controls"
                    | "dsd_mode"
            )
            || k.eq_ignore_ascii_case("volume");
        if !known {
            plan.skipped.push(skip_line(&format!("audio.{k}"), UNKNOWN_WHY));
        }
    }
}

fn plan_quality_fallback(v: &Value, plan: &mut ImportPlan) {
    if v.as_str() == Some("ask") {
        // §5.5: never a silent skip on a daemon.
        adapted_line(
            plan,
            "audio.quality_fallback_behavior",
            v,
            &Value::String("always_fallback".into()),
            "no one to ask on a daemon",
        );
    } else {
        applied_line(plan, "audio.quality_fallback_behavior", v, "");
    }
}

fn plan_audio_machine(
    map: &Map<String, Value>,
    current: &AudioSettings,
    opts: &ImportOptions,
    live: &LiveSystem,
    forced_device: &Option<DeviceChoice>,
    plan: &mut ImportPlan,
) {
    let device_present = map.contains_key("output_device");
    let backend_present = map.contains_key("backend_type");

    let bundle_device: Option<String> = map
        .get("output_device")
        .and_then(|v| v.as_str().filter(|s| !s.is_empty()).map(str::to_string));
    let device_found = bundle_device
        .as_ref()
        .map(|id| live.devices.iter().any(|(d, _)| d == id))
        .unwrap_or(false);
    let device_no_change = device_present && bundle_device == current.output_device;
    let device_is_null = device_present && bundle_device.is_none();

    // Decide the device outcome.
    // device_survives => intent flags may apply; fallback => forces safe defaults.
    let mut fallback = false;
    let device_survives;
    let resolved_device: Option<String>; // None = system default
    let mut device_repick_label: Option<String> = None;

    match forced_device {
        Some(DeviceChoice::Device { id, label }) => {
            resolved_device = Some(id.clone());
            device_survives = true;
            device_repick_label = Some(label.clone());
        }
        Some(DeviceChoice::SystemDefault) => {
            resolved_device = None;
            device_survives = false;
        }
        None => {
            if device_no_change || device_is_null || !device_present {
                resolved_device = bundle_device.clone();
                device_survives = true;
            } else if device_found {
                resolved_device = bundle_device.clone();
                device_survives = true;
            } else {
                // present, changed, not found → needs a pick.
                fallback = true;
                resolved_device = None;
                device_survives = false;
                if !opts.non_tty {
                    plan.device_pick = Some(DevicePick {
                        wanted: bundle_device.clone().unwrap_or_default(),
                        backend: pick_backend_name(map, current),
                        options: live.devices.clone(),
                    });
                }
            }
        }
    }

    // output_device line.
    if device_present {
        let new_val = match &resolved_device {
            Some(id) => Value::String(id.clone()),
            None => Value::Null,
        };
        if resolved_device == bundle_device {
            let why = if device_no_change {
                ""
            } else if device_is_null {
                ""
            } else {
                "found on this machine"
            };
            applied_line(plan, "audio.output_device", &new_val, why);
        } else {
            let old_val = match &bundle_device {
                Some(id) => Value::String(id.clone()),
                None => Value::Null,
            };
            let why = match &device_repick_label {
                Some(label) => format!("re-picked: {label}"),
                None => "device not found on this machine".to_string(),
            };
            adapted_line(plan, "audio.output_device", &old_val, &new_val, &why);
            plan.routing_critical_changed = true;
        }
    }

    // backend_type.
    if backend_present {
        let bundle_backend: Option<AudioBackendType> = map
            .get("backend_type")
            .and_then(|v| serde_json::from_value(v.clone()).ok());
        let bundle_backend_val = map.get("backend_type").cloned().unwrap_or(Value::Null);
        let backend_no_change = bundle_backend == current.backend_type;
        let valid = bundle_backend.is_none()
            || bundle_backend == Some(AudioBackendType::SystemDefault)
            || bundle_backend
                .map(|b| live.backends.iter().any(|x| backend_name(b) == *x))
                .unwrap_or(false);

        if backend_no_change {
            applied_line(plan, "audio.backend_type", &bundle_backend_val, "");
        } else if fallback {
            // non-tty / pre-pick fallback forces system default output.
            let sd = Value::String(backend_name(AudioBackendType::SystemDefault).to_string());
            if bundle_backend == Some(AudioBackendType::SystemDefault) {
                applied_line(plan, "audio.backend_type", &bundle_backend_val, "");
            } else {
                adapted_line(
                    plan,
                    "audio.backend_type",
                    &bundle_backend_val,
                    &sd,
                    "output device unavailable; falling back to system default",
                );
                plan.routing_critical_changed = true;
            }
        } else if valid {
            applied_line(
                plan,
                "audio.backend_type",
                &bundle_backend_val,
                "available on this machine",
            );
            plan.routing_critical_changed = true;
        } else {
            let sd = Value::String(backend_name(AudioBackendType::SystemDefault).to_string());
            adapted_line(
                plan,
                "audio.backend_type",
                &bundle_backend_val,
                &sd,
                "backend not available on this machine",
            );
            plan.routing_critical_changed = true;
        }
    }

    // intent flags — ride the device outcome.
    for flag in AUDIO_INTENT_FLAGS {
        let Some(v) = map.get(*flag) else { continue };
        let bundle_bool = v.as_bool().unwrap_or(false);
        let current_bool = intent_flag_current(current, flag);
        if bundle_bool == current_bool {
            applied_line(plan, &format!("audio.{flag}"), v, "");
        } else if device_survives {
            applied_line(plan, &format!("audio.{flag}"), v, "rides the device");
            plan.routing_critical_changed = true;
        } else {
            adapted_line(
                plan,
                &format!("audio.{flag}"),
                v,
                &Value::Bool(false),
                "reset (no validated device)",
            );
        }
    }

    // ALSA plugin / hardware-volume flag / exact route map — apply only with
    // a validated ALSA device. Imported identities are data only; the active
    // route is probed again before any mixer write.
    let resolved_backend_alsa = resolved_backend_is_alsa(map, current, fallback, forced_device);
    for key in [
        "alsa_plugin",
        "alsa_hardware_volume",
        "alsa_hardware_volume_controls",
    ] {
        let Some(v) = map.get(key) else { continue };
        let no_change = alsa_field_no_change(current, key, v);
        if no_change {
            applied_line(plan, &format!("audio.{key}"), v, "");
        } else if device_survives && resolved_backend_alsa {
            applied_line(plan, &format!("audio.{key}"), v, "rides the ALSA device");
            plan.routing_critical_changed = true;
        } else {
            plan.skipped.push(skip_line(
                &format!("audio.{key}"),
                "applies only with a validated ALSA device",
            ));
        }
    }

    // dsd_mode — no-change short-circuit, else downgrade unless --trust-dsd (§5.3 step 4).
    if let Some(v) = map.get("dsd_mode") {
        let bundle_dsd = v.as_str().unwrap_or("convert");
        if bundle_dsd == current.dsd_mode {
            applied_line(plan, "audio.dsd_mode", v, "");
        } else if matches!(bundle_dsd, "dop" | "native") && (!opts.trust_dsd || fallback) {
            adapted_line(
                plan,
                "audio.dsd_mode",
                v,
                &Value::String("convert".into()),
                "pass --trust-dsd to keep DoP",
            );
        } else {
            applied_line(plan, "audio.dsd_mode", v, "");
        }
    }
}

/// The backend the device options belong to: the bundle's backend when present,
/// else the target's current, else system default (for the §5.4 prompt line).
fn pick_backend_name(map: &Map<String, Value>, current: &AudioSettings) -> String {
    let bundle_backend: Option<AudioBackendType> = map
        .get("backend_type")
        .and_then(|v| serde_json::from_value(v.clone()).ok());
    backend_name(
        bundle_backend
            .or(current.backend_type)
            .unwrap_or(AudioBackendType::SystemDefault),
    )
    .to_string()
}

fn backend_name(b: AudioBackendType) -> &'static str {
    match b {
        AudioBackendType::PipeWire => "PipeWire",
        AudioBackendType::Alsa => "Alsa",
        AudioBackendType::Pulse => "Pulse",
        AudioBackendType::Jack => "Jack",
        AudioBackendType::WasapiExclusive => "WasapiExclusive",
        AudioBackendType::SystemDefault => "SystemDefault",
    }
}

fn intent_flag_current(current: &AudioSettings, flag: &str) -> bool {
    match flag {
        "exclusive_mode" => current.exclusive_mode,
        "dac_passthrough" => current.dac_passthrough,
        "pw_force_bitperfect" => current.pw_force_bitperfect,
        "skip_sink_switch" => current.skip_sink_switch,
        "reserve_dac_while_running" => current.reserve_dac_while_running,
        _ => false,
    }
}

fn alsa_field_no_change(current: &AudioSettings, key: &str, v: &Value) -> bool {
    match key {
        "alsa_hardware_volume" => v.as_bool() == Some(current.alsa_hardware_volume),
        "alsa_hardware_volume_controls" => {
            serde_json::to_value(&current.alsa_hardware_volume_controls)
                .ok()
                .as_ref()
                == Some(v)
        }
        "alsa_plugin" => {
            let cur = serde_json::to_value(current.alsa_plugin).unwrap_or(Value::Null);
            *v == cur
        }
        _ => false,
    }
}

fn resolved_backend_is_alsa(
    map: &Map<String, Value>,
    current: &AudioSettings,
    fallback: bool,
    forced_device: &Option<DeviceChoice>,
) -> bool {
    if fallback || matches!(forced_device, Some(DeviceChoice::SystemDefault)) {
        return false;
    }
    let bundle_backend: Option<AudioBackendType> = map
        .get("backend_type")
        .and_then(|v| serde_json::from_value(v.clone()).ok());
    match bundle_backend {
        Some(b) => b == AudioBackendType::Alsa,
        None => {
            // backend not in the bundle → the target keeps its current backend.
            current.backend_type == Some(AudioBackendType::Alsa)
        }
    }
}

// ---- qconnect (§2.4) ----
fn plan_qconnect(value: &Value, target: &ProfilePaths, plan: &mut ImportPlan) {
    let Some(map) = value.as_object() else {
        return;
    };
    let _ = target;
    for (k, v) in map {
        match k.as_str() {
            "device_name" => applied_line(plan, "qconnect.device_name", v, ""),
            "startup_mode" => match v.as_str() {
                Some("remember_last") => adapted_line(
                    plan,
                    "qconnect.startup_mode",
                    v,
                    &Value::String("on".into()),
                    "daemon has no last-state tracking",
                ),
                _ => applied_line(plan, "qconnect.startup_mode", v, ""),
            },
            "device_uuid" => plan.skipped.push(skip_line(
                "qconnect.device_uuid",
                "never imported (identity clone — two nodes would fight)",
            )),
            "last_known_state" => plan.skipped.push(skip_line(
                "qconnect.last_known_state",
                "never imported (runtime state)",
            )),
            _ if k.eq_ignore_ascii_case("volume") => {
                plan.skipped.push(skip_line(&format!("qconnect.{k}"), VOLUME_SKIP_WHY));
            }
            _ => plan.skipped.push(skip_line(&format!("qconnect.{k}"), UNKNOWN_WHY)),
        }
    }
}

// ---- integrations.scrobblers (§2.5) ----
const SCROBBLER_PORTABLE: &[&str] = &[
    "enabled",
    "lastfm_enabled",
    "lastfm_username",
    "listenbrainz_enabled",
    "listenbrainz_username",
];
const SCROBBLER_SECRET: &[&str] = &["lastfm_session_key", "listenbrainz_token"];

fn plan_integrations(value: &Value, opts: &ImportOptions, uid_will_exist: bool, plan: &mut ImportPlan) {
    let Some(scrob) = value.get("scrobblers").and_then(Value::as_object) else {
        // Unknown integration domain.
        if let Some(map) = value.as_object() {
            for k in map.keys() {
                plan.skipped.push(skip_line(&format!("integrations.{k}"), UNKNOWN_WHY));
            }
        }
        return;
    };

    for (k, v) in scrob {
        let full = format!("integrations.scrobblers.{k}");
        if !uid_will_exist {
            plan.skipped.push(skip_line(
                &full,
                "no user on this daemon yet — run qbzd login first, or import with --include-auth",
            ));
            continue;
        }
        if SCROBBLER_PORTABLE.contains(&k.as_str()) {
            applied_line(plan, &full, v, "");
        } else if SCROBBLER_SECRET.contains(&k.as_str()) {
            if opts.include_auth {
                applied_secret_line(plan, &full, v);
            } else {
                plan.skipped
                    .push(skip_line(&full, "secrets require --include-auth"));
            }
        } else if k == "ui_collapsed" {
            plan.skipped
                .push(skip_line(&full, "not imported (desktop UI state)"));
        } else {
            plan.skipped.push(skip_line(&full, UNKNOWN_WHY));
        }
    }
}

// ---- library_folders — P0 daemon always skips (§2.6) ----
fn plan_library_folders(value: &Value, plan: &mut ImportPlan) {
    let count = value.as_array().map(|a| a.len()).unwrap_or(0);
    plan.skipped.push(PlanLine {
        key: format!("library_folders ({count} folder{})", if count == 1 { "" } else { "s" }),
        old: None,
        new: String::new(),
        why: "no local library on qbzd v1".to_string(),
    });
}

// ---- auth — SECRET, double gate (§2.7, §3) ----
fn plan_auth(value: &Value, opts: &ImportOptions, plan: &mut ImportPlan) {
    let Some(map) = value.as_object() else {
        return;
    };
    plan.bundle_user_id = map.get("user_id").and_then(Value::as_u64);

    let token = map.get("user_auth_token").and_then(Value::as_str);
    match token {
        Some(t) if !t.is_empty() => {
            if opts.include_auth {
                plan.auth_token = Some(t.to_string());
                // The applied line is added by the CLI after validation
                // (the token is validated BEFORE any write, §5.3 step 5).
            } else {
                plan.skipped.push(skip_line(
                    "auth.user_auth_token",
                    "secrets require --include-auth",
                ));
            }
        }
        _ => {}
    }
}

// ============================ apply write dispatch ============================

fn apply_audio_writes(data_root: &Path, writes: &[(&str, &Value)]) -> Result<(), String> {
    let store = AudioSettingsStore::new_at(data_root)?;
    for (key, value) in writes {
        match *key {
            "backend_type" => {
                let b: Option<AudioBackendType> = serde_json::from_value((*value).clone())
                    .map_err(|e| format!("backend_type: {e}"))?;
                store.set_backend_type(b)?;
            }
            "output_device" => {
                let d = value.as_str();
                store.set_output_device(d)?;
            }
            "alsa_plugin" => {
                let p = serde_json::from_value((*value).clone())
                    .map_err(|e| format!("alsa_plugin: {e}"))?;
                store.set_alsa_plugin(p)?;
            }
            "alsa_hardware_volume" => store.set_alsa_hardware_volume(as_bool(value))?,
            "alsa_hardware_volume_controls" => {
                let controls = serde_json::from_value((*value).clone())
                    .map_err(|e| format!("alsa_hardware_volume_controls: {e}"))?;
                store.set_alsa_hardware_volume_controls(controls)?;
            }
            "exclusive_mode" => store.set_exclusive_mode(as_bool(value))?,
            "dac_passthrough" => store.set_dac_passthrough(as_bool(value))?,
            "pw_force_bitperfect" => store.set_pw_force_bitperfect(as_bool(value))?,
            "skip_sink_switch" => store.set_skip_sink_switch(as_bool(value))?,
            "reserve_dac_while_running" => store.set_reserve_dac_while_running(as_bool(value))?,
            "dsd_mode" => store.set_dsd_mode(value.as_str().unwrap_or("convert"))?,
            "stream_first_track" => store.set_stream_first_track(as_bool(value))?,
            "stream_buffer_seconds" => {
                store.set_stream_buffer_seconds(value.as_u64().unwrap_or(2) as u8)?
            }
            "playback_cache" => {
                let policy = serde_json::from_value((*value).clone()).map_err(|e| format!("playback_cache: {e}"))?;
                store.set_playback_cache(&policy)?;
            }
            "streaming_only" => store.set_streaming_only(as_bool(value))?,
            "limit_quality_to_device" => store.set_limit_quality_to_device(as_bool(value))?,
            "preferred_sample_rate" => {
                store.set_sample_rate(value.as_u64().map(|r| r as u32))?
            }
            "normalization_enabled" => store.set_normalization_enabled(as_bool(value))?,
            "normalization_target_lufs" => {
                store.set_normalization_target_lufs(value.as_f64().unwrap_or(-14.0) as f32)?
            }
            "normalization_prevent_clipping" => {
                store.set_normalization_prevent_clipping(as_bool(value))?
            }
            "gapless_enabled" => store.set_gapless_enabled(as_bool(value))?,
            "allow_quality_fallback" => store.set_allow_quality_fallback(as_bool(value))?,
            "sync_audio_on_startup" => store.set_sync_audio_on_startup(as_bool(value))?,
            "quality_fallback_behavior" => {
                store.set_quality_fallback_behavior(value.as_str().unwrap_or("always_fallback"))?
            }
            other => log::warn!("[bundle] apply: unhandled audio key {other}"),
        }
    }
    Ok(())
}

fn apply_playback_writes(data_root: &Path, writes: &[(&str, &Value)]) -> Result<(), String> {
    let store = PlaybackPreferencesStore::new_at(data_root)?;
    for (key, value) in writes {
        match *key {
            "autoplay_mode" => {
                let mode = serde_json::from_value((*value).clone())
                    .map_err(|e| format!("autoplay_mode: {e}"))?;
                store.set_autoplay_mode(mode)?;
            }
            "show_context_icon" => store.set_show_context_icon(as_bool(value))?,
            "persist_session" => store.set_persist_session(as_bool(value))?,
            "resume_playback_position" => store.set_resume_playback_position(as_bool(value))?,
            other => log::warn!("[bundle] apply: unhandled playback key {other}"),
        }
    }
    Ok(())
}

fn apply_prefs_quality(data_root: &Path, value: &Value) -> Result<(), String> {
    let mut prefs = daemon_prefs::load_at(data_root);
    if let Some(q) = value.as_str() {
        prefs.streaming_quality = q.to_string();
    }
    daemon_prefs::save_at(&prefs, data_root)
}

fn apply_qconnect_writes(data_root: &Path, writes: &[(&str, &Value)]) -> Result<(), String> {
    let db = data_root.join("qconnect_settings.db");
    for (key, value) in writes {
        match *key {
            "device_name" => {
                let name = value.as_str().filter(|s| !s.is_empty());
                qconnect_kv_write(&db, "device_name", name)?;
            }
            "startup_mode" => {
                qconnect_kv_write(&db, "startup_mode", value.as_str())?;
            }
            other => log::warn!("[bundle] apply: unhandled qconnect key {other}"),
        }
    }
    Ok(())
}

fn apply_scrobbler_writes(
    data_root: &Path,
    uid: u64,
    writes: &[(&str, &Value)],
) -> Result<(), String> {
    let dir = data_root.join(format!("users/{uid}"));
    let store = ScrobblerSettingsStore::new_at(&dir)?;
    let mut current = store.get_settings()?;
    for (key, value) in writes {
        match *key {
            "enabled" => store.set_enabled(as_bool(value))?,
            "lastfm_enabled" => store.set_lastfm_enabled(as_bool(value))?,
            "lastfm_username" => {
                current.lastfm_username = value.as_str().unwrap_or("").to_string();
                store.set_lastfm_session(&current.lastfm_session_key, &current.lastfm_username)?;
            }
            "lastfm_session_key" => {
                current.lastfm_session_key = value.as_str().unwrap_or("").to_string();
                store.set_lastfm_session(&current.lastfm_session_key, &current.lastfm_username)?;
            }
            "listenbrainz_enabled" => store.set_listenbrainz_enabled(as_bool(value))?,
            "listenbrainz_username" => {
                current.listenbrainz_username = value.as_str().unwrap_or("").to_string();
                store.set_listenbrainz_token(
                    &current.listenbrainz_token,
                    &current.listenbrainz_username,
                )?;
            }
            "listenbrainz_token" => {
                current.listenbrainz_token = value.as_str().unwrap_or("").to_string();
                store.set_listenbrainz_token(
                    &current.listenbrainz_token,
                    &current.listenbrainz_username,
                )?;
            }
            other => log::warn!("[bundle] apply: unhandled scrobbler key {other}"),
        }
    }
    Ok(())
}

fn persist_auth(target: &ProfilePaths, token: &str, uid: u64) -> Result<(), String> {
    qbz_credentials::save_oauth_token_at(&target.config_root, token)?;
    // last_user_id under the DAEMON root (NEVER the desktop global path — the
    // daemon must not touch ~/.local/share/qbz; 04 §5.7 cites the desktop fn
    // only for the flat-file format).
    write_last_user_id(&target.data_root, uid)?;
    std::fs::create_dir_all(target.data_root.join(format!("users/{uid}")))
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn as_bool(v: &Value) -> bool {
    v.as_bool().unwrap_or(false)
}

// ============================ store readers (side-effect free) ============================

/// Read the current audio settings WITHOUT creating the DB (so `plan`/dry-run
/// never write). Returns `None` when the store file is absent (caller uses
/// `AudioSettings::default()`).
fn read_audio_settings(data_root: &Path) -> Option<AudioSettings> {
    if !data_root.join("audio_settings.db").exists() {
        return None;
    }
    AudioSettingsStore::new_at(data_root)
        .and_then(|s| s.get_settings())
        .ok()
}

fn read_playback_prefs(data_root: &Path) -> Option<PlaybackPreferences> {
    if !data_root.join("playback_preferences.db").exists() {
        return None;
    }
    PlaybackPreferencesStore::new_at(data_root)
        .and_then(|s| s.get_preferences())
        .ok()
}

fn playback_to_json(p: &PlaybackPreferences) -> Value {
    serde_json::to_value(p).unwrap_or(Value::Null)
}

fn read_scrobblers(data_root: &Path, uid: u64, include_auth: bool) -> Option<Value> {
    let dir = data_root.join(format!("users/{uid}"));
    if !dir.join("scrobbler_settings.db").exists() {
        return None;
    }
    let store = ScrobblerSettingsStore::new_at(&dir).ok()?;
    let s = store.get_settings().ok()?;
    let mut obj = Map::new();
    obj.insert("enabled".into(), Value::Bool(s.enabled));
    obj.insert("lastfm_enabled".into(), Value::Bool(s.lastfm_enabled));
    obj.insert("lastfm_username".into(), Value::String(s.lastfm_username));
    obj.insert(
        "listenbrainz_enabled".into(),
        Value::Bool(s.listenbrainz_enabled),
    );
    obj.insert(
        "listenbrainz_username".into(),
        Value::String(s.listenbrainz_username),
    );
    // Secrets only with --include-auth; otherwise empty (§2.5).
    let lastfm_key = if include_auth { s.lastfm_session_key } else { String::new() };
    let lb_token = if include_auth { s.listenbrainz_token } else { String::new() };
    obj.insert("lastfm_session_key".into(), Value::String(lastfm_key));
    obj.insert("listenbrainz_token".into(), Value::String(lb_token));
    Some(Value::Object(obj))
}

fn read_library_folders(data_root: &Path) -> Option<Value> {
    // Global desktop library DB (`<data>/qbz/library.db`); network_fs is
    // derived at runtime on the desktop and is cosmetic for the daemon (which
    // always skips this domain), so it is emitted false here.
    let db = data_root.join("library.db");
    if !db.exists() {
        return None;
    }
    let conn = rusqlite::Connection::open(&db).ok()?;
    let mut stmt = conn
        .prepare("SELECT path FROM library_folders ORDER BY path")
        .ok()?;
    let rows = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .ok()?
        .filter_map(Result::ok);
    let arr: Vec<Value> = rows
        .map(|path| {
            let mut o = Map::new();
            o.insert("path".into(), Value::String(path));
            o.insert("network_fs".into(), Value::Bool(false));
            Value::Object(o)
        })
        .collect();
    if arr.is_empty() {
        None
    } else {
        Some(Value::Array(arr))
    }
}

fn read_qconnect_domain(data_root: &Path) -> Option<Value> {
    let db = data_root.join("qconnect_settings.db");
    if !db.exists() {
        return None;
    }
    let mut obj = Map::new();
    if let Some(name) = qconnect_kv_read(&db, "device_name") {
        obj.insert("device_name".into(), Value::String(name));
    }
    if let Some(mode) = qconnect_kv_read(&db, "startup_mode") {
        obj.insert("startup_mode".into(), Value::String(mode));
    }
    if obj.is_empty() {
        None
    } else {
        Some(Value::Object(obj))
    }
}

fn read_ui_prefs_streaming_quality(data_root: &Path) -> Option<String> {
    // Minimal serde read of ~90-field ui_prefs.json — serde ignores the rest
    // (§2.3; the 890-line ui_prefs.rs is NOT moved).
    #[derive(Deserialize)]
    struct MinimalUiPrefs {
        streaming_quality: Option<String>,
    }
    let text = std::fs::read_to_string(data_root.join("ui_prefs.json")).ok()?;
    let p: MinimalUiPrefs = serde_json::from_str(&text).ok()?;
    p.streaming_quality
}

// ---- qconnect KV (self-contained rusqlite — the engine must be desktop-callable) ----
fn qconnect_kv_read(db: &Path, key: &str) -> Option<String> {
    let conn = rusqlite::Connection::open(db).ok()?;
    conn.query_row(
        "SELECT value FROM settings WHERE key = ?1",
        rusqlite::params![key],
        |row| row.get::<_, String>(0),
    )
    .ok()
    .filter(|v| !v.trim().is_empty())
}

fn qconnect_kv_write(db: &Path, key: &str, value: Option<&str>) -> Result<(), String> {
    let conn = rusqlite::Connection::open(db).map_err(|e| e.to_string())?;
    conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=NORMAL; PRAGMA busy_timeout=1000;")
        .map_err(|e| e.to_string())?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL)",
    )
    .map_err(|e| e.to_string())?;
    match value {
        Some(v) => conn
            .execute(
                "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
                rusqlite::params![key, v],
            )
            .map(|_| ())
            .map_err(|e| e.to_string()),
        None => conn
            .execute(
                "DELETE FROM settings WHERE key = ?1",
                rusqlite::params![key],
            )
            .map(|_| ())
            .map_err(|e| e.to_string()),
    }
}

// ---- last_user_id under an arbitrary root (daemon-safe) ----
fn read_last_user_id(data_root: &Path) -> Option<u64> {
    std::fs::read_to_string(data_root.join("last_user_id"))
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
}

fn write_last_user_id(data_root: &Path, uid: u64) -> Result<(), String> {
    std::fs::create_dir_all(data_root).map_err(|e| e.to_string())?;
    std::fs::write(data_root.join("last_user_id"), uid.to_string()).map_err(|e| e.to_string())
}

// ---- decrypted token load (export --include-auth) ----
fn load_decrypted_token(
    source: &ExportSource,
    paths: &ProfilePaths,
) -> Result<Option<String>, BundleError> {
    match source {
        ExportSource::Desktop => match qbz_credentials::load_oauth_token() {
            Ok(Some(t)) => Ok(Some(t)),
            Ok(None) => {
                // Distinguish "no token" from "present but undecryptable" (IV1:
                // the portal secret is bound to the desktop session, §4.1).
                if paths.config_root.join(".qbz-oauth-token").exists() {
                    Err(BundleError::TokenDecryptFailed)
                } else {
                    Ok(None)
                }
            }
            Err(_) => {
                if paths.config_root.join(".qbz-oauth-token").exists() {
                    Err(BundleError::TokenDecryptFailed)
                } else {
                    Ok(None)
                }
            }
        },
        ExportSource::Daemon(_) => {
            match qbz_credentials::load_oauth_token_at(&paths.config_root) {
                Ok(Some(t)) => Ok(Some(t)),
                Ok(None) => Ok(None),
                Err(e) => Err(BundleError::Io(e)),
            }
        }
    }
}

// ---- desktop paths + misc ----
fn desktop_paths() -> ProfilePaths {
    let config_root = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("qbz");
    let data_root = dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("qbz");
    ProfilePaths {
        config_root,
        data_root,
    }
}

/// The machine's name, for user-facing device labels.
///
/// `HOSTNAME` is a SHELL variable rather than an environment one on most Linux
/// setups, hence the file fallback. Windows exports `COMPUTERNAME` and has no
/// `/etc/hostname`, so every Windows install used to land on the same
/// "unknown" and collide on Qobuz Connect.
///
/// The single implementation: `qbz-qt` and `qbzd` both call it and keep only
/// their own last-resort word, which differ on purpose.
pub fn hostname() -> String {
    for var in ["HOSTNAME", "COMPUTERNAME"] {
        if let Ok(h) = std::env::var(var) {
            let h = h.trim();
            if !h.is_empty() {
                return h.to_string();
            }
        }
    }
    if let Ok(h) = std::fs::read_to_string("/etc/hostname") {
        let h = h.trim();
        if !h.is_empty() {
            return h.to_string();
        }
    }
    "unknown".to_string()
}

#[cfg(test)]
mod hostname_tests {
    /// ONE test, not two: the environment is process-global and cargo runs
    /// tests on parallel threads, so a sibling that sets HOSTNAME races a
    /// sibling that clears it. Splitting these failed exactly that way.
    #[test]
    fn computername_is_the_windows_source_and_hostname_still_wins() {
        let saved_host = std::env::var("HOSTNAME").ok();
        let saved_computer = std::env::var("COMPUTERNAME").ok();

        std::env::remove_var("HOSTNAME");
        std::env::set_var("COMPUTERNAME", "WINBOX");
        assert_eq!(super::hostname(), "WINBOX", "Windows has only COMPUTERNAME");

        std::env::set_var("HOSTNAME", "gentoo-box");
        assert_eq!(
            super::hostname(),
            "gentoo-box",
            "HOSTNAME keeps precedence, so Linux behaviour does not move"
        );

        match saved_host {
            Some(v) => std::env::set_var("HOSTNAME", v),
            None => std::env::remove_var("HOSTNAME"),
        }
        match saved_computer {
            Some(v) => std::env::set_var("COMPUTERNAME", v),
            None => std::env::remove_var("COMPUTERNAME"),
        }
    }
}

fn now_rfc3339() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

/// The suggested export filename `qbz-settings-YYYYMMDD.qbzb` (04 §1).
pub fn default_filename() -> String {
    format!("qbz-settings-{}.qbzb", chrono::Utc::now().format("%Y%m%d"))
}

/// Serialize a bundle to `path`, ALWAYS mode 0600 — fail rather than fall back
/// to a wider mode (04 §6). Shared by the CLI and the P1 desktop modal.
pub fn write_bundle_file(path: &Path, bundle: &Bundle) -> Result<(), BundleError> {
    let json = bundle.to_json_string()?;
    #[cfg(unix)]
    {
        use std::io::Write as _;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true).mode(0o600);
        let mut f = opts
            .open(path)
            .map_err(|e| BundleError::Io(format!("could not create {}: {e}", path.display())))?;
        f.write_all(json.as_bytes())
            .map_err(|e| BundleError::Io(format!("could not write {}: {e}", path.display())))?;
        // Enforce 0600 even if the file pre-existed with a wider mode.
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(|e| {
            BundleError::Io(format!(
                "refusing to leave {} more permissive than 0600: {e}",
                path.display()
            ))
        })?;
        Ok(())
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, json.as_bytes())
            .map_err(|e| BundleError::Io(format!("could not write {}: {e}", path.display())))
    }
}

#[cfg(test)]
mod tests;
