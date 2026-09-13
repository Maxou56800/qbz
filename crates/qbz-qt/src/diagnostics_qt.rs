//! Settings > Developer — the system-diagnostics panel and the body of the
//! bug-report bundle.
//!
//! Port of `crates/qbz/src/diagnostics.rs` (the Slint controller) shaped like
//! `log_viewer_qt.rs`: statics for the flags, a `Mutex` cache for the export,
//! one `snapshot_doc()` serialized into ONE JSON property
//! (`QbzShell.diagnosticsJson`), and a closed panel that publishes nothing.
//!
//! It has TWO consumers, and that is the point:
//!   1. `DiagnosticsPanel.qml` — the seven saved-vs-runtime tables.
//!   2. `log_viewer_qt::bundle_text()` — [`report_markdown`] is the body every
//!      Copy-bundle and every paste.rs upload now carries. Before this landed
//!      a Qt bug report shipped EIGHT header fields where the Slint one ships
//!      ~70, so every issue filed from a Qt build arrived with no kernel, no
//!      distro, no install method, no GPU, no exclusive-mode/DAC/sample-rate
//!      state and no playback context.
//!
//! ── WHERE THE CODE LIVES, AND WHY IT IS NOT SHARED ────────────────────────
//!
//! The producers below (`gather`, the row builders, `collect_output_sinks`,
//! `active_sink_format`, `redact_id_like`) originated in the retired frontend
//! but are now the sole application implementation. Everything that already
//! lives in a shared crate is called, not copied: `qbz_app::diagnostics`
//! (`runtime_diagnostics` / `system_info` / `detect_graphics_runtime`),
//! `qbz_audio::output_sinks::list_output_sinks`, and Qt's own `renderer_qt` /
//! `settings_qt` / `cast_qt` / `qconnect_qt`.
//!
//! ── ROWS THE REFERENCE HAS THAT THIS PANEL DELIBERATELY DOES NOT ──────────
//!
//! CUT, because in a Qt process they can only ever render `—` (their libraries
//! are not mapped, so `detect_loaded_lib_version` returns `None`) AND their
//! "Saved" column would come out of a `graphics.json` this binary never reads
//! or writes — presenting stale foreign state as this app's configuration:
//!
//!   System      WebKit2GTK, GTK
//!   Graphics    Hardware Acceleration, Force DMA-BUF, Force X11, GSK Renderer,
//!               GDK Scale, GDK DPI Scale, Compositing Mode, Using Fallback
//!   Environment WEBKIT_DISABLE_DMABUF_RENDERER, WEBKIT_DISABLE_COMPOSITING_MODE,
//!               GDK_BACKEND, GSK_RENDERER
//!
//! Do not "restore parity" by adding them back — every one of them is a Tauri-era
//! fossil that Slint carried forward verbatim, and this port's standing rule is
//! no dead rows (`DeveloperSettings.qml:6`).
//!
//! ALSO CUT: `UI Loop Latency`. `crates/qbz/src/ui_watchdog.rs` measures Slint's
//! `upgrade_in_event_loop` dispatch latency; a Qt analogue would be a new module,
//! not a port.
//!
//! REPLACED, not dropped — the Qt truths that stand where a GDK/GSK row stood:
//! `Renderer (Qt)` (saved pref vs the resolved RHI api), `GPU Adapters` (Qt's
//! own Vulkan-device order, which `QT_VK_PHYSICAL_DEVICE_INDEX` addresses),
//! `GPU Tier`, and the
//! `QSG_RHI_BACKEND` / `QT_QUICK_BACKEND` / `QT_VK_PHYSICAL_DEVICE_INDEX`
//! environment rows — which this binary SETS itself (`renderer_qt.rs:491`), so
//! they are the most load-bearing env vars it has.
//!
//! Per-row LABELS are plain untranslated Rust strings, exactly as in the
//! reference (`DiagnosticsPanel.slint:11-13`); only section titles, column
//! headers and buttons go through `@tr` — QML-side. ZERO new msgids.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};

use cxx_qt_lib::QString;
use serde::Serialize;
use serde_json::{json, Value};

use crate::shell_bridge::ui;

/// Whether the master collapsible is expanded. A closed panel publishes
/// NOTHING (the `log_viewer_qt.rs:189` discipline).
static OPEN: AtomicBool = AtomicBool::new(false);
static LOADED: AtomicBool = AtomicBool::new(false);
static LOADING: AtomicBool = AtomicBool::new(false);
/// The export button's transient confirmation, flashed for 1500 ms — the
/// reference's number (`crates/qbz/src/diagnostics.rs:217`).
static COPIED: AtomicBool = AtomicBool::new(false);
static CAST_SCANNING: AtomicBool = AtomicBool::new(false);

static ERROR: LazyLock<Mutex<String>> = LazyLock::new(|| Mutex::new(String::new()));
static SECTIONS: LazyLock<Mutex<Sections>> = LazyLock::new(|| Mutex::new(Sections::default()));
/// Export base cached on each `refresh()`; `castScan` + `exportedAt` are merged
/// in at export time, like the reference.
static EXPORT: LazyLock<Mutex<Option<Value>>> = LazyLock::new(|| Mutex::new(None));
static LAST_CAST: LazyLock<Mutex<Option<Value>>> = LazyLock::new(|| Mutex::new(None));

/// How long the on-demand cast scan waits before reading the device lists.
/// The reference's number (`crates/qbz/src/diagnostics.rs:246`).
const CAST_SCAN_SECS: u64 = 10;

// ---------------------------------------------------------------------------
// The published document
// ---------------------------------------------------------------------------

/// One diagnostics row. `status`: 0 info | 1 match | 2 mismatch — the glyph
/// mapping `DiagnosticsPanel.qml` renders (`·` / `✓` / `✗`).
#[derive(Serialize, Clone, Default)]
struct Row {
    label: String,
    saved: String,
    runtime: String,
    status: i32,
}

#[derive(Serialize, Clone, Default)]
struct Sections {
    system: Vec<Row>,
    playback: Vec<Row>,
    qconnect: Vec<Row>,
    cast: Vec<Row>,
    audio: Vec<Row>,
    graphics: Vec<Row>,
    env: Vec<Row>,
}

#[derive(Serialize)]
struct Doc {
    open: bool,
    loaded: bool,
    loading: bool,
    error: String,
    #[serde(rename = "appVersion")]
    app_version: String,
    copied: bool,
    #[serde(rename = "castScanning")]
    cast_scanning: bool,
    #[serde(flatten)]
    sections: Sections,
}

/// The full shape, so the bridge can seed the property with a document every
/// binding in the QML can read on the pre-publish frame.
pub fn empty_doc_json() -> String {
    serde_json::to_string(&Doc {
        open: false,
        loaded: false,
        loading: false,
        error: String::new(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        copied: false,
        cast_scanning: false,
        sections: Sections::default(),
    })
    .unwrap_or_else(|_| "{}".into())
}

fn snapshot_doc() -> Doc {
    Doc {
        open: OPEN.load(Ordering::SeqCst),
        loaded: LOADED.load(Ordering::SeqCst),
        loading: LOADING.load(Ordering::SeqCst),
        error: ERROR.lock().unwrap_or_else(|e| e.into_inner()).clone(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        copied: COPIED.load(Ordering::SeqCst),
        cast_scanning: CAST_SCANNING.load(Ordering::SeqCst),
        sections: SECTIONS.lock().unwrap_or_else(|e| e.into_inner()).clone(),
    }
}

fn publish_now() {
    let json = serde_json::to_string(&snapshot_doc()).unwrap_or_else(|_| "{}".into());
    ui(move |mut b| {
        b.as_mut()
            .set_diagnostics_json(QString::from(json.as_str()))
    });
}

/// Publish the document — but only while the panel is expanded. A collapsed
/// panel is not reading, and every publish wakes the Qt thread and re-parses
/// a document that can carry a hundred rows.
pub fn publish() {
    if !OPEN.load(Ordering::SeqCst) {
        return;
    }
    publish_now();
}

/// The master collapsible's open state. The reference keeps this private to the
/// component (`DiagnosticsPanel.slint:190`); here it has to reach Rust because
/// that is what gates [`publish`].
pub fn set_open(open: bool) {
    OPEN.store(open, Ordering::SeqCst);
    // Unconditional: the closing publish is the one that tells the view the
    // panel is closed, and it is the last one it will get.
    publish_now();
}

// ---------------------------------------------------------------------------
// Gather
// ---------------------------------------------------------------------------

/// Everything one refresh reads, in one struct — so the panel and the markdown
/// report render off the SAME gather instead of drifting.
struct Gathered {
    diag: qbz_app::diagnostics::RuntimeDiagnostics,
    sys: qbz_app::diagnostics::SystemInfo,
    active_output: Option<String>,
    available_outputs: Vec<String>,
    /// `(rate, format)` of the live default sink, e.g. `("44100 Hz", "s32le · 2ch")`.
    active_fmt: Option<(String, String)>,
    /// Settings > Appearance preference vs the RHI api that actually resolved.
    renderer_saved: String,
    renderer_runtime: String,
    gpu_adapters: String,
    gpu_tier: bool,
    pb: qbz_player::PlaybackState,
    track: Option<qbz_models::QueueTrack>,
    qc: crate::qconnect_qt::QconnectDiagSnapshot,
    catalog: CatalogDiagnostics,
}

/// The blocking half: settings stores, `/proc`, `/sys`, `pactl` and the CPAL
/// sink enumeration. Never on the async path.
struct Blocking {
    diag: qbz_app::diagnostics::RuntimeDiagnostics,
    sys: qbz_app::diagnostics::SystemInfo,
    active_output: Option<String>,
    available_outputs: Vec<String>,
    active_fmt: Option<(String, String)>,
    renderer_saved: String,
    renderer_runtime: String,
    gpu_adapters: String,
    gpu_tier: bool,
    catalog: CatalogDiagnostics,
}

#[derive(Clone, Serialize)]
struct CatalogDiagnostics {
    state: String,
    reason: String,
    generation: Option<u64>,
    tracks: Option<u64>,
    bytes: Option<u64>,
    runtime_check: String,
    runtime_ok: bool,
    fts_activation_verified: bool,
    routes: String,
}

fn gather_blocking() -> Blocking {
    // The process-wide audio store handle, NOT a second `AudioSettingsStore`:
    // two handles over one SQLite file is the defect `settings_qt::audio_settings`
    // exists to prevent (settings_qt.rs:68-80).
    let audio = crate::settings_qt::audio_settings();

    // Graphics + developer settings are passed as DEFAULTS on purpose. They
    // back `graphics.json`, which this binary never reads or writes, so their
    // values would be foreign state — and NO row below is sourced from them
    // (see the CUT list in the file header). They are here only because
    // `runtime_diagnostics` needs the three structs to produce the fields that
    // ARE Qt-true: the GPU vendor flags, the GPU name, the desktop
    // environment, the Wayland/VM flags and the environment variables.
    let graphics = qbz_app::settings::graphics::GraphicsSettings::default();
    let developer = qbz_app::settings::developer::DeveloperSettings::default();
    let gfx = qbz_app::diagnostics::detect_graphics_runtime(&graphics, false);
    let diag =
        qbz_app::diagnostics::runtime_diagnostics(&qbz_app::diagnostics::DiagnosticsInputs {
            audio: &audio,
            graphics: &graphics,
            developer: &developer,
            gfx,
            app_version: env!("CARGO_PKG_VERSION"),
        });
    let sys = qbz_app::diagnostics::system_info();
    let (active_output, available_outputs, active_fmt) = collect_output_sinks();

    let renderer_saved = crate::settings_qt::pref_str("renderer", "auto");
    let renderer_runtime = crate::renderer_qt::active_api();
    let gpus = crate::renderer_qt::gpus();
    let gpu_adapters = if gpus.is_empty() {
        "—".to_string()
    } else {
        gpus.iter()
            .map(|g| format!("[{}] {}", g.index, g.label()))
            .collect::<Vec<_>>()
            .join(", ")
    };

    Blocking {
        diag,
        sys,
        active_output,
        available_outputs,
        active_fmt,
        renderer_saved,
        renderer_runtime,
        gpu_adapters,
        gpu_tier: crate::renderer_qt::gpu_tier(),
        catalog: gather_catalog_diagnostics(),
    }
}

fn gather_catalog_diagnostics() -> CatalogDiagnostics {
    let album_mode = crate::local_library_qt::album_mode();
    let route = |requested: bool, unavailable: &str, needs_folder: bool| {
        if requested && (!needs_folder || album_mode == "folder") {
            "catalog".to_string()
        } else if requested {
            "legacy(metadata-mode)".to_string()
        } else {
            format!("legacy({unavailable})")
        }
    };
    let routes = format!(
        "tracks={}, albums={}, artists={}, genres=legacy(native-surface-not-migrated)",
        route(
            crate::local_tracks_model_qt::requested(),
            "feature-disabled-or-session-failed",
            false,
        ),
        route(
            crate::local_albums_model_qt::requested(),
            "feature-disabled-or-session-failed",
            true,
        ),
        route(
            crate::local_artists_model_qt::requested(),
            "feature-disabled-or-session-failed",
            true,
        ),
    );
    let Some(locations) = crate::local_catalog_qt::locations() else {
        return CatalogDiagnostics {
            state: "unavailable".to_string(),
            reason: "missing-data-directory".to_string(),
            generation: None,
            tracks: None,
            bytes: None,
            runtime_check: "not available".to_string(),
            runtime_ok: false,
            fts_activation_verified: false,
            routes,
        };
    };
    match qbz_local_catalog::BootstrapLayout::new(&locations.catalog_dir).open_active() {
        qbz_local_catalog::ActiveCatalog::Fallback(reason) => CatalogDiagnostics {
            state: "fallback".to_string(),
            reason: match reason {
                qbz_local_catalog::FallbackReason::NoManifest => "no-manifest",
                qbz_local_catalog::FallbackReason::InvalidManifest(_) => "invalid-manifest",
                qbz_local_catalog::FallbackReason::MissingGeneration(_) => "missing-generation",
                qbz_local_catalog::FallbackReason::CatalogRejected(_) => "catalog-rejected",
            }
            .to_string(),
            generation: None,
            tracks: None,
            bytes: None,
            runtime_check: "not available".to_string(),
            runtime_ok: false,
            fts_activation_verified: false,
            routes,
        },
        qbz_local_catalog::ActiveCatalog::Ready { catalog, manifest } => {
            let stats = catalog.stats();
            let integrity = catalog.runtime_integrity_check();
            let (runtime_check, runtime_ok) = match integrity {
                Ok(report) => {
                    let ok = report.sqlite_ok
                        && report.foreign_key_violations == 0
                        && report.materialized_views_ok;
                    (
                        format!(
                            "sqlite={} foreign-keys={} materialized={}",
                            if report.sqlite_ok { "ok" } else { "failed" },
                            report.foreign_key_violations,
                            if report.materialized_views_ok {
                                "ok"
                            } else {
                                "failed"
                            }
                        ),
                        ok,
                    )
                }
                Err(_) => ("check failed".to_string(), false),
            };
            CatalogDiagnostics {
                state: "active".to_string(),
                reason: String::new(),
                generation: Some(manifest.active_generation),
                tracks: stats.as_ref().ok().map(|value| value.track_count),
                bytes: stats
                    .as_ref()
                    .ok()
                    .map(|value| value.page_size_bytes.saturating_mul(value.page_count)),
                runtime_check,
                runtime_ok,
                fts_activation_verified: true,
                routes,
            }
        }
    }
}

fn format_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    let bytes = bytes as f64;
    if bytes >= GIB {
        format!("{:.2} GiB", bytes / GIB)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes / MIB)
    } else if bytes >= KIB {
        format!("{:.1} KiB", bytes / KIB)
    } else {
        format!("{} B", bytes as u64)
    }
}

async fn gather() -> Result<Gathered, String> {
    let b = tokio::task::spawn_blocking(gather_blocking)
        .await
        .map_err(|e| format!("{e}"))?;

    let runtime = crate::app();
    let pb = runtime.core().get_playback_state();
    let track = runtime.core().current_track().await;

    // LIVE QConnect snapshot (no discovery; default when not running).
    let qc = match crate::qconnect_qt::service() {
        Some(s) => s.diagnostics_snapshot().await,
        None => Default::default(),
    };

    Ok(Gathered {
        diag: b.diag,
        sys: b.sys,
        active_output: b.active_output,
        available_outputs: b.available_outputs,
        active_fmt: b.active_fmt,
        renderer_saved: b.renderer_saved,
        renderer_runtime: b.renderer_runtime,
        gpu_adapters: b.gpu_adapters,
        gpu_tier: b.gpu_tier,
        pb,
        track,
        qc,
        catalog: b.catalog,
    })
}

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Rebuild every section. Re-entrancy is guarded by an atomic swap — the same
/// shape `log_viewer_qt::upload` uses — because a refresh re-runs the `pactl`
/// shell-outs and the CPAL enumeration, and the reference debounces it with
/// nothing but its `loading` flag.
pub fn refresh() {
    if LOADING.swap(true, Ordering::SeqCst) {
        return;
    }
    publish();
    crate::spawn(async move {
        match gather().await {
            Ok(g) => {
                let sections = build_sections(&g);
                let export = build_export_json(&g);
                {
                    let mut slot = SECTIONS.lock().unwrap_or_else(|e| e.into_inner());
                    // The cast rows are NOT part of a refresh — they are the
                    // on-demand scan's output and must survive one.
                    let cast = std::mem::take(&mut slot.cast);
                    *slot = sections;
                    slot.cast = cast;
                }
                *EXPORT.lock().unwrap_or_else(|e| e.into_inner()) = Some(export);
                ERROR.lock().unwrap_or_else(|e| e.into_inner()).clear();
                LOADED.store(true, Ordering::SeqCst);
            }
            Err(e) => {
                log::warn!("[qbz-qt] diagnostics: gather failed: {e}");
                // Untranslated, exactly like the reference
                // (`crates/qbz/src/diagnostics.rs:119` sets a plain literal):
                // translating it would be a NEW msgid in eight catalogs for a
                // line only reachable when a settings store panicked.
                *ERROR.lock().unwrap_or_else(|e| e.into_inner()) =
                    "Failed to read diagnostics".to_string();
            }
        }
        LOADING.store(false, Ordering::SeqCst);
        publish();
    });
}

/// Serialize the cached snapshot (+ the last cast scan + `exportedAt`) to the
/// clipboard, then flash the button's confirmation for 1500 ms.
pub fn export_clipboard() {
    let base = EXPORT.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let Some(mut value) = base else {
        return;
    };
    if let Some(map) = value.as_object_mut() {
        let cast = LAST_CAST.lock().unwrap_or_else(|e| e.into_inner()).clone();
        map.insert("castScan".to_string(), cast.unwrap_or(Value::Null));
        map.insert("exportedAt".to_string(), Value::String(now_utc_rfc3339()));
    }
    crate::share_qt::copy_to_clipboard(serde_json::to_string_pretty(&value).unwrap_or_default());

    COPIED.store(true, Ordering::SeqCst);
    publish();
    crate::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        COPIED.store(false, Ordering::SeqCst);
        publish();
    });
}

/// On-demand Cast discovery scan: reuse the existing `CastService`, wait
/// [`CAST_SCAN_SECS`], read the device lists, then stop discovery ONLY if the
/// picker is not relying on it (stopping under a live picker would kill its
/// device list).
pub fn cast_scan() {
    if CAST_SCANNING.swap(true, Ordering::SeqCst) {
        return;
    }
    publish();
    crate::spawn(async move {
        let svc = crate::cast_qt::service();
        svc.start_discovery().await;
        tokio::time::sleep(std::time::Duration::from_secs(CAST_SCAN_SECS)).await;
        let (cc, dl, devices) = svc.diag_devices().await;

        let mut rows = vec![
            row("Chromecast devices", "—", &cc.to_string(), 0),
            row("DLNA devices", "—", &dl.to_string(), 0),
        ];
        let mut device_json: Vec<Value> = Vec::with_capacity(devices.len());
        for (protocol, name) in devices {
            rows.push(row(&format!("• {protocol}"), "—", &name, 0));
            device_json.push(json!({ "name": name, "protocol": protocol }));
        }
        *LAST_CAST.lock().unwrap_or_else(|e| e.into_inner()) = Some(json!({
            "chromecastCount": cc,
            "dlnaCount": dl,
            "devices": device_json,
        }));
        SECTIONS.lock().unwrap_or_else(|e| e.into_inner()).cast = rows;

        CAST_SCANNING.store(false, Ordering::SeqCst);
        publish();

        if !crate::cast_qt::picker_open() {
            svc.stop_discovery().await;
        }
    });
}

/// The COMPLETE markdown diagnostics report — the body of every log bundle and
/// every paste.rs upload (`log_viewer_qt::bundle_text`).
///
/// It re-runs the gather rather than reading the panel's cache, exactly like
/// the reference (`crates/qbz/src/diagnostics.rs:314`): a bug report must
/// describe the machine at the moment it is filed, and the panel may never
/// have been opened at all.
pub async fn report_markdown() -> String {
    match gather().await {
        Ok(g) => build_report(&g),
        Err(e) => format!(
            "# qbz diagnostics\n\n- **Version:** {}\n\nFailed to gather diagnostics: {e}\n",
            env!("CARGO_PKG_VERSION")
        ),
    }
}

// ---------------------------------------------------------------------------
// Row builders (1:1 with the reference's, minus the CUT rows)
// ---------------------------------------------------------------------------

fn row(label: &str, saved: &str, runtime: &str, status: i32) -> Row {
    Row {
        label: label.to_string(),
        saved: saved.to_string(),
        runtime: runtime.to_string(),
        status,
    }
}

/// `ON`/`OFF`, mirroring the Tauri `bool()` helper.
fn yn(value: bool) -> &'static str {
    if value {
        "ON"
    } else {
        "OFF"
    }
}

/// `Some -> value`, `None -> "—"`, mirroring the Tauri `str()` helper.
fn opt(value: &Option<String>) -> String {
    value.clone().unwrap_or_else(|| "—".to_string())
}

/// Format a kHz value without a trailing ".0" (96.0 -> "96", 44.1 -> "44.1").
fn trim_khz(khz: f64) -> String {
    if khz.fract().abs() < f64::EPSILON {
        format!("{}", khz as i64)
    } else {
        format!("{khz:.1}")
    }
}

fn env_opt(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| "—".to_string())
}

fn build_sections(g: &Gathered) -> Sections {
    Sections {
        system: build_system_rows(&g.sys, &g.catalog),
        playback: build_playback_rows(&g.pb, g.track.as_ref()),
        qconnect: build_qconnect_rows(&g.qc),
        cast: Vec::new(),
        audio: build_audio_rows(g),
        graphics: build_graphics_rows(g),
        env: build_env_rows(&g.diag),
    }
}

fn build_system_rows(
    s: &qbz_app::diagnostics::SystemInfo,
    catalog: &CatalogDiagnostics,
) -> Vec<Row> {
    let mut rows = vec![
        row("OS", "—", &s.os, 0),
        row("Arch", "—", &s.arch, 0),
        row("Kernel", "—", &opt(&s.kernel_version), 0),
        row("Distro", "—", &opt(&s.distro_pretty_name), 0),
        row("Distro ID", "—", &opt(&s.distro_id), 0),
        row("Distro Version", "—", &opt(&s.distro_version_id), 0),
        row("Install Method", "—", &s.install_method, 0),
    ];
    if let Some(runtime) = &s.flatpak_runtime {
        rows.push(row(
            "Flatpak Runtime",
            "—",
            &format!("{} {}", runtime, opt(&s.flatpak_runtime_version)),
            0,
        ));
    }
    // WebKit2GTK / GTK are CUT here — see the file header.
    rows.push(row("glibc", "—", &opt(&s.glibc_version), 0));
    rows.push(row("ALSA", "—", &opt(&s.alsa_version), 0));
    rows.push(row("PipeWire", "—", &opt(&s.pipewire_version), 0));
    rows.push(row("PulseAudio", "—", &opt(&s.pulseaudio_version), 0));
    rows.push(row(
        "Local Catalog",
        "—",
        &if catalog.reason.is_empty() {
            catalog.state.clone()
        } else {
            format!("{} ({})", catalog.state, catalog.reason)
        },
        if catalog.state == "active" { 1 } else { 2 },
    ));
    rows.push(row(
        "Local Catalog Generation",
        "—",
        &catalog
            .generation
            .map(|value| value.to_string())
            .unwrap_or_else(|| "—".to_string()),
        0,
    ));
    rows.push(row(
        "Local Catalog Tracks",
        "—",
        &catalog
            .tracks
            .map(|value| value.to_string())
            .unwrap_or_else(|| "—".to_string()),
        0,
    ));
    rows.push(row(
        "Local Catalog Size",
        "—",
        &catalog
            .bytes
            .map(format_bytes)
            .unwrap_or_else(|| "—".to_string()),
        0,
    ));
    rows.push(row(
        "Local Catalog Runtime Check",
        "—",
        &catalog.runtime_check,
        if catalog.runtime_ok { 1 } else { 2 },
    ));
    rows.push(row(
        "Local Catalog FTS",
        "—",
        if catalog.fts_activation_verified {
            "verified at activation"
        } else {
            "not active"
        },
        if catalog.fts_activation_verified {
            1
        } else {
            0
        },
    ));
    rows.push(row("Local Library Routes", "—", &catalog.routes, 0));
    rows
}

fn build_audio_rows(g: &Gathered) -> Vec<Row> {
    let d = &g.diag;
    let selected_mixer = d
        .audio_alsa_hardware_volume_probe
        .as_ref()
        .and_then(|probe| probe.route_key.as_ref())
        .and_then(|route| d.audio_alsa_hardware_volume_controls.get(route))
        .map(ToString::to_string)
        .unwrap_or_else(|| "—".to_string());
    let mixer_probe = d
        .audio_alsa_hardware_volume_probe
        .as_ref()
        .map(|probe| {
            let valid = probe.valid_candidates().count();
            let rejected = probe.candidates.len().saturating_sub(valid);
            if let Some(error) = &probe.error {
                format!("{:?}: {}", error.kind, error.message)
            } else {
                format!("{valid} valid · {rejected} rejected")
            }
        })
        .unwrap_or_else(|| "—".to_string());
    let sample_rate = match d.audio_preferred_sample_rate {
        Some(hz) => format!("{hz} Hz"),
        None => "Auto".to_string(),
    };

    // Output Device: saved id (may be a stale/unplugged DAC) vs the live active
    // output. Match (1) when the active equals OR is contained/suffixed by the
    // saved value; mismatch (2) when an active device exists but differs (so the
    // stale-saved-vs-live discrepancy is visible); info (0) when no live device.
    let saved_output = opt(&d.audio_output_device);
    let active_output = g.active_output.as_deref();
    let output_runtime = active_output.unwrap_or("—");
    let output_status = match active_output {
        Some(active) => {
            if saved_output == active
                || saved_output.contains(active)
                || saved_output.ends_with(active)
            {
                1
            } else {
                2
            }
        }
        None => 0,
    };
    let available_runtime = if g.available_outputs.is_empty() {
        "—".to_string()
    } else {
        g.available_outputs.join(", ")
    };
    let active_rate = g
        .active_fmt
        .as_ref()
        .map(|(r, _)| r.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("—");
    let active_fmt = g
        .active_fmt
        .as_ref()
        .map(|(_, f)| f.as_str())
        .filter(|s| !s.is_empty())
        .unwrap_or("—");

    vec![
        row(
            "Output Device",
            &saved_output,
            output_runtime,
            output_status,
        ),
        row("Backend", &opt(&d.audio_backend_type), "—", 0),
        row("Exclusive Mode", yn(d.audio_exclusive_mode), "—", 0),
        row("DAC Passthrough", yn(d.audio_dac_passthrough), "—", 0),
        row("Preferred Sample Rate", &sample_rate, active_rate, 0),
        row("Active Format", "—", active_fmt, 0),
        row("ALSA Plugin", &opt(&d.audio_alsa_plugin), "—", 0),
        row("ALSA HW Volume", yn(d.audio_alsa_hardware_volume), "—", 0),
        row("ALSA HW Control", &selected_mixer, &mixer_probe, 0),
        row("Normalization", yn(d.audio_normalization_enabled), "—", 0),
        row(
            "Normalization Target",
            &format!("{} LUFS", d.audio_normalization_target_lufs),
            "—",
            0,
        ),
        row("Normalization Clipping Guard", yn(d.audio_normalization_prevent_clipping), "—", 0),
        row("Gapless", yn(d.audio_gapless_enabled), "—", 0),
        row(
            "PW Force Bitperfect",
            yn(d.audio_pw_force_bitperfect),
            "—",
            0,
        ),
        row(
            "Stream Buffer",
            &format!("{}s", d.audio_stream_buffer_seconds),
            "—",
            0,
        ),
        row("Streaming Only", yn(d.audio_streaming_only), "—", 0),
        row("Available Outputs", "—", &available_runtime, 0),
    ]
}

fn build_graphics_rows(g: &Gathered) -> Vec<Row> {
    let d = &g.diag;
    vec![
        // The Qt analogue of the reference's "Renderer (Slint)": the saved
        // Settings > Appearance preference vs the RHI api that actually
        // resolved at startup (`renderer_qt::active_api`).
        row("Renderer (Qt)", &g.renderer_saved, &g.renderer_runtime, 0),
        // Stands where the reference's `renderer_decision_summary()` adapter
        // string does. NOT byte-identical: this is the actual Qt-owned Vulkan
        // instance order addressed by `QT_VK_PHYSICAL_DEVICE_INDEX`, not
        // wgpu's or an independently-created Vulkan instance's adapter list.
        row("GPU Adapters", "—", &g.gpu_adapters, 0),
        // Shaders + the dynamic background need this; false means the scenes
        // stay hidden and reduce-motion is forced on, which is exactly the
        // condition a bug report needs to carry.
        row(
            "GPU Tier",
            "—",
            yn(g.gpu_tier),
            if g.gpu_tier { 0 } else { 2 },
        ),
        row(
            "GPU",
            "—",
            if d.runtime_gpu_name.is_empty() {
                "Unknown"
            } else {
                &d.runtime_gpu_name
            },
            0,
        ),
        row(
            "GPU: NVIDIA",
            "—",
            if d.runtime_has_nvidia {
                "Detected"
            } else {
                "No"
            },
            0,
        ),
        row(
            "GPU: Intel",
            "—",
            if d.runtime_has_intel {
                "Detected"
            } else {
                "No"
            },
            0,
        ),
        row(
            "GPU: AMD",
            "—",
            if d.runtime_has_amd { "Detected" } else { "No" },
            0,
        ),
        row(
            "Desktop Environment",
            "—",
            if d.runtime_desktop_environment.is_empty() {
                "Unknown"
            } else {
                &d.runtime_desktop_environment
            },
            0,
        ),
        row(
            "Wayland",
            "—",
            if d.runtime_is_wayland {
                "Yes"
            } else {
                "No (X11)"
            },
            0,
        ),
        row("VM", "—", if d.runtime_is_vm { "Yes" } else { "No" }, 0),
    ]
}

fn build_env_rows(d: &qbz_app::diagnostics::RuntimeDiagnostics) -> Vec<Row> {
    vec![
        row(
            "LIBGL_ALWAYS_SOFTWARE",
            "—",
            &opt(&d.env_libgl_always_software),
            0,
        ),
        row("WAYLAND_DISPLAY", "—", &opt(&d.env_wayland_display), 0),
        row("XDG_SESSION_TYPE", "—", &opt(&d.env_xdg_session_type), 0),
        // The Qt renderer env vars, in place of the reference's GDK/GSK pair.
        // This binary SETS the first two itself (main.rs:2626-2653,
        // renderer_qt.rs:491), so what they hold at runtime is the single most
        // useful line in a rendering bug report.
        row("QSG_RHI_BACKEND", "—", &env_opt("QSG_RHI_BACKEND"), 0),
        row("QT_QUICK_BACKEND", "—", &env_opt("QT_QUICK_BACKEND"), 0),
        row(
            "QT_VK_PHYSICAL_DEVICE_INDEX",
            "—",
            &env_opt("QT_VK_PHYSICAL_DEVICE_INDEX"),
            0,
        ),
    ]
}

fn build_playback_rows(
    pb: &qbz_player::PlaybackState,
    track: Option<&qbz_models::QueueTrack>,
) -> Vec<Row> {
    let volume_percent = (pb.volume * 100.0).round() as i64;
    let bit_depth = track
        .and_then(|t| t.bit_depth)
        .map(|d| format!("{d}-bit"))
        .unwrap_or_else(|| "—".to_string());
    let sample_rate = track
        .and_then(|t| t.sample_rate)
        .map(|r| format!("{} kHz", trim_khz(r)))
        .unwrap_or_else(|| "—".to_string());
    let is_local = match track {
        Some(t) => yn(t.is_local).to_string(),
        None => "—".to_string(),
    };

    vec![
        row("Playing", "—", yn(pb.is_playing), 0),
        row("Volume", "—", &format!("{volume_percent}%"), 0),
        row(
            "Position / Duration",
            "—",
            &format!("{}s / {}s", pb.position, pb.duration),
            0,
        ),
        row("Has Track", "—", yn(track.is_some()), 0),
        row("Track Title", "—", &opt(&track.map(|t| t.title.clone())), 0),
        row(
            "Track Artist",
            "—",
            &opt(&track.map(|t| t.artist.clone())),
            0,
        ),
        row("Track Album", "—", &opt(&track.map(|t| t.album.clone())), 0),
        row(
            "Track Source",
            "—",
            &opt(&track.and_then(|t| t.source.clone())),
            0,
        ),
        row("Track Is Local", "—", &is_local, 0),
        // No quality/format field on QueueTrack — "—" is faithful to the data,
        // exactly as in the reference.
        row("Track Quality", "—", "—", 0),
        row("Track Format", "—", "—", 0),
        row("Track Bit Depth", "—", &bit_depth, 0),
        row("Track Sample Rate", "—", &sample_rate, 0),
    ]
}

fn build_qconnect_rows(q: &crate::qconnect_qt::QconnectDiagSnapshot) -> Vec<Row> {
    let role = if q.role.is_empty() { "none" } else { q.role };
    let last_error = q
        .last_error
        .as_deref()
        .map(redact_id_like)
        .unwrap_or_else(|| "—".to_string());
    vec![
        row("Running", "—", yn(q.running), 0),
        row("Transport Connected", "—", yn(q.transport_connected), 0),
        row("Has Endpoint", "—", yn(q.has_endpoint), 0),
        row("Role", "—", role, 0),
        row("Active Renderer", "—", &opt(&q.active_name), 0),
        row("Renderer Brand", "—", &opt(&q.active_brand), 0),
        row("Renderer Model", "—", &opt(&q.active_model), 0),
        row("Visible Renderers", "—", &q.renderer_count.to_string(), 0),
        row("Last Error", "—", &last_error, 0),
    ]
}

// ---------------------------------------------------------------------------
// Markdown report (the log bundle's body)
// ---------------------------------------------------------------------------

/// Append a `- **key:** value` markdown bullet (one self-contained line, so it
/// renders correctly without relying on trailing-whitespace hard breaks).
fn md_line(out: &mut String, key: &str, value: &str) {
    out.push_str("- **");
    out.push_str(key);
    out.push_str(":** ");
    out.push_str(value);
    out.push('\n');
}

/// Render one section's rows as bullets. The panel and the report therefore
/// cannot disagree about which rows exist — the whole reason the CUT list is
/// applied in ONE place (the row builders) and not twice.
fn md_section(out: &mut String, title: &str, rows: &[Row], show_saved: bool) {
    out.push_str("\n## ");
    out.push_str(title);
    out.push_str("\n\n");
    for r in rows {
        let value = if show_saved && r.saved != "—" && r.runtime != "—" {
            format!("saved {} / runtime {}", r.saved, r.runtime)
        } else if r.runtime != "—" {
            r.runtime.clone()
        } else {
            r.saved.clone()
        };
        md_line(out, &r.label, &value);
    }
}

fn build_report(g: &Gathered) -> String {
    let s = build_sections(g);
    let mut out = String::new();
    out.push_str("# qbz diagnostics\n\n");
    md_line(&mut out, "Version", env!("CARGO_PKG_VERSION"));
    md_line(&mut out, "Frontend", "Qt/QML");
    md_line(&mut out, "Generated", &now_utc_rfc3339());

    md_section(&mut out, "System", &s.system, false);
    md_section(&mut out, "Audio", &s.audio, true);
    md_section(&mut out, "Graphics", &s.graphics, true);
    md_section(&mut out, "Environment", &s.env, false);
    md_section(&mut out, "Playback", &s.playback, false);
    md_section(&mut out, "Qobuz Connect", &s.qconnect, false);
    // The Cast section is deliberately absent: it exists only after the user
    // presses "Scan for devices", and a bug-report bundle must never trigger a
    // 10 s network discovery on its own.
    out
}

// ---------------------------------------------------------------------------
// Export JSON
// ---------------------------------------------------------------------------

/// The clipboard export.
///
/// DELIBERATE DIVERGENCE from the reference, which flattens the whole
/// `RuntimeDiagnostics` struct into the object
/// (`crates/qbz/src/diagnostics.rs:160-164`). Doing that here would emit
/// `gfxHardwareAcceleration`, `devForceDmabuf`, `envWebkit*` and friends — the
/// exact fossil fields the panel CUT, filled from defaults this binary never
/// persisted. An export that carries invented configuration is worse than one
/// that carries less, so this builds the object from the fields that are
/// genuinely true of a Qt process.
fn build_export_json(g: &Gathered) -> Value {
    let d = &g.diag;
    let s = &g.sys;
    let (active_rate, active_fmt) = match &g.active_fmt {
        Some((r, f)) => (Some(r.clone()), Some(f.clone())),
        None => (None, None),
    };
    json!({
        "appVersion": env!("CARGO_PKG_VERSION"),
        "frontend": "qt",
        "systemInfo": {
            "os": s.os,
            "arch": s.arch,
            "kernelVersion": s.kernel_version,
            "distroId": s.distro_id,
            "distroVersionId": s.distro_version_id,
            "distroPrettyName": s.distro_pretty_name,
            "installMethod": s.install_method,
            "flatpakRuntime": s.flatpak_runtime,
            "flatpakRuntimeVersion": s.flatpak_runtime_version,
            "glibcVersion": s.glibc_version,
            "alsaVersion": s.alsa_version,
            "pipewireVersion": s.pipewire_version,
            "pulseaudioVersion": s.pulseaudio_version,
        },
        "localCatalog": g.catalog,
        "audio": {
            "outputDevice": d.audio_output_device,
            "backendType": d.audio_backend_type,
            "exclusiveMode": d.audio_exclusive_mode,
            "dacPassthrough": d.audio_dac_passthrough,
            "preferredSampleRate": d.audio_preferred_sample_rate,
            "alsaPlugin": d.audio_alsa_plugin,
            "alsaHardwareVolume": d.audio_alsa_hardware_volume,
            "alsaHardwareVolumeControls": d.audio_alsa_hardware_volume_controls,
            "alsaHardwareVolumeProbe": d.audio_alsa_hardware_volume_probe,
            "normalizationEnabled": d.audio_normalization_enabled,
            "normalizationTargetLufs": d.audio_normalization_target_lufs,
            "normalizationPreventClipping": d.audio_normalization_prevent_clipping,
            "gaplessEnabled": d.audio_gapless_enabled,
            "pwForceBitperfect": d.audio_pw_force_bitperfect,
            "streamBufferSeconds": d.audio_stream_buffer_seconds,
            "streamingOnly": d.audio_streaming_only,
            "activeOutput": g.active_output,
            "activeRate": active_rate,
            "activeFormat": active_fmt,
            "availableOutputs": g.available_outputs,
        },
        "graphics": {
            "rendererSaved": g.renderer_saved,
            "rendererRuntime": g.renderer_runtime,
            "gpuAdapters": g.gpu_adapters,
            "gpuTier": g.gpu_tier,
            "gpuName": d.runtime_gpu_name,
            "hasNvidia": d.runtime_has_nvidia,
            "hasIntel": d.runtime_has_intel,
            "hasAmd": d.runtime_has_amd,
            "desktopEnvironment": d.runtime_desktop_environment,
            "isWayland": d.runtime_is_wayland,
            "isVm": d.runtime_is_vm,
        },
        "environment": {
            "LIBGL_ALWAYS_SOFTWARE": d.env_libgl_always_software,
            "WAYLAND_DISPLAY": d.env_wayland_display,
            "XDG_SESSION_TYPE": d.env_xdg_session_type,
            "QSG_RHI_BACKEND": std::env::var("QSG_RHI_BACKEND").ok(),
            "QT_QUICK_BACKEND": std::env::var("QT_QUICK_BACKEND").ok(),
            "QT_VK_PHYSICAL_DEVICE_INDEX": std::env::var("QT_VK_PHYSICAL_DEVICE_INDEX").ok(),
        },
        "playback": {
            "isPlaying": g.pb.is_playing,
            "volumePercent": (g.pb.volume * 100.0).round() as i64,
            "positionSecs": g.pb.position,
            "durationSecs": g.pb.duration,
            "hasTrack": g.track.is_some(),
            "trackTitle": g.track.as_ref().map(|t| t.title.clone()),
            "trackArtist": g.track.as_ref().map(|t| t.artist.clone()),
            "trackAlbum": g.track.as_ref().map(|t| t.album.clone()),
            "trackQuality": Value::Null,
            "trackFormat": Value::Null,
            "trackBitDepth": g.track.as_ref().and_then(|t| t.bit_depth),
            "trackSamplingRate": g.track.as_ref().and_then(|t| t.sample_rate),
            "trackIsLocal": g.track.as_ref().map(|t| t.is_local),
            "trackSource": g.track.as_ref().and_then(|t| t.source.clone()),
        },
        "qconnect": {
            "running": g.qc.running,
            "transportConnected": g.qc.transport_connected,
            "hasEndpoint": g.qc.has_endpoint,
            "lastError": g.qc.last_error.as_deref().map(redact_id_like),
            "role": if g.qc.role.is_empty() { "none" } else { g.qc.role },
            "activeRendererName": g.qc.active_name,
            "activeRendererBrand": g.qc.active_brand,
            "activeRendererModel": g.qc.active_model,
            "rendererCount": g.qc.renderer_count,
        },
    })
}

// ---------------------------------------------------------------------------
// Live output sinks
// ---------------------------------------------------------------------------

/// Query the live output sinks (BLOCKING — CPAL enumeration). Must be called
/// inside a `spawn_blocking`. `active_output` is the description (fallback
/// name) of the default sink; `available_outputs` is every sink's.
/// READ-ONLY — never touches the protected audio backend.
fn collect_output_sinks() -> (Option<String>, Vec<String>, Option<(String, String)>) {
    let label = |s: &qbz_audio::output_sinks::OutputSinkInfo| -> String {
        if s.description.is_empty() {
            s.name.clone()
        } else {
            s.description.clone()
        }
    };
    let fmt = active_sink_format();
    match qbz_audio::output_sinks::list_output_sinks() {
        Ok(sinks) => {
            let active = sinks.iter().find(|s| s.is_default).map(&label);
            let available = sinks.iter().map(&label).collect();
            (active, available, fmt)
        }
        Err(e) => {
            log::warn!("[qbz-qt] diagnostics: list_output_sinks failed: {e}");
            (None, Vec::new(), fmt)
        }
    }
}

/// Best-effort LIVE sample format of the active (default) output sink, parsed
/// from `pactl list sinks short`. Returns `(rate, format)` like
/// `("44100 Hz", "s32le · 2ch")` — the rate the device is ACTUALLY running at
/// right now (vs the saved "Preferred Sample Rate"). Linux/PipeWire/Pulse only;
/// `None` when pactl is unavailable (a Flatpak/Snap sandbox without it simply
/// degrades to "—", same as the reference). READ-ONLY.
/// W14, Windows only: `pactl` is a Pulse/PipeWire tool and does not exist on
/// Windows, so the row degrades to the same "-" the missing-pactl path already
/// produced -- without the spawn. macOS and the BSDs keep probing: a Homebrew
/// or ports `pactl` there is real and can answer.
#[cfg(windows)]
fn active_sink_format() -> Option<(String, String)> {
    None
}

#[cfg(not(windows))]
fn active_sink_format() -> Option<(String, String)> {
    use std::process::Command;
    let default = Command::new("pactl")
        .arg("get-default-sink")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string());

    let out = Command::new("pactl")
        .args(["list", "sinks", "short"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?;

    // sample-spec token like "s32le 2ch 44100Hz" -> ("44100 Hz", "s32le · 2ch").
    let parse_spec = |spec: &str| -> (String, String) {
        let (mut rate, mut chans, mut fmt) = (String::new(), String::new(), String::new());
        for tok in spec.split_whitespace() {
            if let Some(hz) = tok.strip_suffix("Hz") {
                rate = format!("{hz} Hz");
            } else if tok.ends_with("ch") {
                chans = tok.to_string();
            } else {
                fmt = tok.to_string();
            }
        }
        let format = match (fmt.is_empty(), chans.is_empty()) {
            (false, false) => format!("{fmt} · {chans}"),
            (false, true) => fmt,
            (true, false) => chans,
            (true, true) => spec.trim().to_string(),
        };
        (rate, format)
    };

    // Prefer the default sink; fall back to the first RUNNING sink.
    let mut running: Option<(String, String)> = None;
    for line in text.lines() {
        let cols: Vec<&str> = line.split('\t').collect();
        if cols.len() < 5 {
            continue;
        }
        let (name, spec, state) = (cols[1], cols[3], cols[4]);
        if let Some(d) = &default {
            if name == d {
                return Some(parse_spec(spec));
            }
        }
        if state.eq_ignore_ascii_case("RUNNING") && running.is_none() {
            running = Some(parse_spec(spec));
        }
    }
    running
}

// ---------------------------------------------------------------------------
// Redaction + time
// ---------------------------------------------------------------------------

/// Redact UUID + long-hex substrings. Applied to the QConnect `Last Error`,
/// which is the one diagnostics field that can carry a session id. Operates on
/// chars so it is UTF-8 safe.
///
/// The ring's own `qbz_log::redact` still runs over every log line in the
/// bundle; this is the diagnostics-side complement, same as the reference.
fn redact_id_like(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect();
    let n = chars.len();
    let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    let mut out = String::with_capacity(value.len());
    let mut i = 0;
    while i < n {
        // UUID shape (8-4-4-4-12 hex), word-boundary delimited.
        if uuid_at(&chars, i)
            && (i == 0 || !is_word(chars[i - 1]))
            && (i + 36 >= n || !is_word(chars[i + 36]))
        {
            out.push_str("<uuid>");
            i += 36;
            continue;
        }
        // A maximal word token that is entirely hex and >= 32 chars long.
        if chars[i].is_ascii_hexdigit() && (i == 0 || !is_word(chars[i - 1])) {
            let mut j = i;
            while j < n && is_word(chars[j]) {
                j += 1;
            }
            if j - i >= 32 && chars[i..j].iter().all(|c| c.is_ascii_hexdigit()) {
                out.push_str("<hex>");
                i = j;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// Whether a 36-char `8-4-4-4-12` hex UUID starts at `i`.
fn uuid_at(chars: &[char], i: usize) -> bool {
    let groups = [8usize, 4, 4, 4, 12];
    let mut p = i;
    for (gi, &len) in groups.iter().enumerate() {
        for _ in 0..len {
            if p >= chars.len() || !chars[p].is_ascii_hexdigit() {
                return false;
            }
            p += 1;
        }
        if gi < 4 {
            if p >= chars.len() || chars[p] != '-' {
                return false;
            }
            p += 1;
        }
    }
    true
}

/// `YYYY-MM-DDTHH:MM:SSZ`. The reference uses `chrono`; this crate does not
/// carry it, and pulling a dependency for one timestamp is not worth it —
/// `build.rs` already solves the same problem the same way (Hinnant's
/// days→civil, `format_ymd`).
fn now_utc_rfc3339() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = secs.div_euclid(86_400);
    let tod = secs.rem_euclid(86_400);
    let (h, m, s) = (tod / 3600, (tod % 3600) / 60, tod % 60);

    // days_from_civil inverse (Hinnant, "chrono-Compatible Low-Level Date
    // Algorithms") — the same routine as `build.rs::format_ymd`.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if mo <= 2 { y + 1 } else { y };
    format!("{year:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}Z")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_uuid_and_long_hex() {
        let s = "session 550e8400-e29b-41d4-a716-446655440000 token \
                 0123456789abcdef0123456789abcdef ok";
        let out = redact_id_like(s);
        assert!(out.contains("<uuid>"), "{out}");
        assert!(out.contains("<hex>"), "{out}");
        assert!(out.contains("session"));
        assert!(out.contains("ok"));
    }

    #[test]
    fn leaves_short_hex_alone() {
        // 8 hex chars (a SONAME-ish short id) is below the 32-char threshold.
        assert_eq!(redact_id_like("abc123 deadbeef end"), "abc123 deadbeef end");
    }

    #[test]
    fn trims_whole_khz_but_keeps_fractions() {
        assert_eq!(trim_khz(96.0), "96");
        assert_eq!(trim_khz(44.1), "44.1");
    }

    #[test]
    fn catalog_size_uses_binary_units_without_exposing_a_path() {
        assert_eq!(format_bytes(900), "900 B");
        assert_eq!(format_bytes(1_536), "1.5 KiB");
        assert_eq!(format_bytes(2 * 1024 * 1024), "2.0 MiB");
    }

    #[test]
    fn rfc3339_shape() {
        let ts = now_utc_rfc3339();
        assert_eq!(ts.len(), 20, "{ts}");
        assert!(ts.ends_with('Z'), "{ts}");
        assert_eq!(&ts[4..5], "-");
        assert_eq!(&ts[10..11], "T");
    }

    #[test]
    fn empty_doc_carries_every_section_key() {
        let v: serde_json::Value = serde_json::from_str(&empty_doc_json()).expect("valid json");
        for key in [
            "open",
            "loaded",
            "loading",
            "error",
            "appVersion",
            "copied",
            "castScanning",
            "system",
            "playback",
            "qconnect",
            "cast",
            "audio",
            "graphics",
            "env",
        ] {
            assert!(v.get(key).is_some(), "seed document is missing `{key}`");
        }
    }

    /// The report renders off the SAME rows the panel does, so a row cut in
    /// one place is cut in both. This pins the section headings and the
    /// saved/runtime rendering rule.
    #[test]
    fn md_section_renders_saved_and_runtime() {
        let rows = vec![
            row("Output Device", "hw:1,0", "USB DAC", 2),
            row("Kernel", "—", "7.1.6", 0),
            row("Exclusive Mode", "ON", "—", 0),
        ];
        let mut out = String::new();
        md_section(&mut out, "Audio", &rows, true);
        assert!(out.starts_with("\n## Audio\n\n"), "{out}");
        assert!(out.contains("- **Output Device:** saved hw:1,0 / runtime USB DAC"));
        assert!(out.contains("- **Kernel:** 7.1.6"));
        assert!(out.contains("- **Exclusive Mode:** ON"));
    }
}
