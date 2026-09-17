// crates/qbzd/src/daemon.rs — the `qbzd run` boot sequence (01-architecture.md
// §8.1, NORMATIVE order), the NeedsAuth-stays-up state machine (§6.2) and the
// graceful shutdown (§8.2). Later tasks splice into the numbered steps: the
// playback driver (T4) at step 10, the HTTP server (T6) at step 11, QConnect
// (T9/T10) at step 12. Until they land the daemon boots a playable core and
// parks on signals — API-less but fully diagnosable in-process.
use std::sync::{Arc, Mutex};

use qbz_app::playback_driver::{self, DriverDeps};
use qbz_app::settings::daemon_prefs;
use qbz_app::shell::AppRuntime;
use qbz_core::CoreError;
use qbz_models::{CoreEvent, UserSession};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use crate::adapter::DaemonAdapter;
use crate::config::QbzdConfig;
use crate::lock::{InstanceLock, LockError};
use crate::paths::ProfileRoots;
use crate::state::{AuthState, DaemonShared, LatchedErrors, QconnectStatus};

/// The composed runtime handoff produced by [`boot`] and consumed by later
/// tasks: T4 spawns the playback driver on `runtime` + `shared`, T6 serves
/// `bus` over HTTP/SSE, T9/T10 wire QConnect. Held alive by [`run`] through the
/// signal park so the core stays up.
#[allow(dead_code)] // fields are the seam later tasks (T6/T9/T10) read.
pub struct BootedRuntime {
    pub runtime: Arc<AppRuntime<DaemonAdapter>>,
    pub shared: Arc<Mutex<DaemonShared>>,
    pub bus: broadcast::Sender<CoreEvent>,
    /// Background session-restore retry (network-class boot failure only). Held
    /// so shutdown can abort+join it BEFORE releasing the audio device: it holds
    /// an `Arc<AppRuntime>` clone, so leaving it running would keep the Player
    /// alive past `drop(booted)` and break the #521 clock-release ordering (§8.2).
    pub auth_retry: Option<JoinHandle<()>>,
}

/// `qbzd run` — boot the daemon in the foreground, park on signals, shut down
/// gracefully. Returns the process exit code (0 = clean shutdown). `warns` are
/// the unknown-key warnings surfaced by [`QbzdConfig::load`] in `main`.
pub async fn run(roots: ProfileRoots, cfg: QbzdConfig, warns: Vec<String>, orbit: bool) -> Result<i32, String> {
    // 1. argv parse happened in main(). 2. logging — to its OWN file, never
    //    the desktop app's (a shared qbz.log was rotated from under the GUI):
    qbz_log::install_named(&cfg.log.level, "qbzd.log");
    // A headless daemon that dies of a signal leaves even less behind than
    // the GUI does: no window, no user watching. Same reporter, same reason
    // (qbz-log/src/fatal.rs).
    qbz_log::install_fatal_signal_reporter();
    // 2b. Host memory profile (issue #660: qbzd OOM-killed on a 1 GB Pi 3B):
    //     one-shot Normal/LowMemory detection, pushed into the player as the
    //     initial-buffer cap, the L1 cache budget and the low-memory class
    //     gate. Must run BEFORE boot() builds the AppRuntime (and with it the
    //     Player + its L1 cache); after the logger so the profile's own info
    //     line lands in the log.
    {
        let profile = qbz_core::system_capabilities::memory_profile();
        qbz_player::player::apply_memory_tuning(
            profile.class == qbz_core::system_capabilities::MemoryClass::LowMemory,
            profile.audio_cache_l1_max_bytes,
            profile.max_initial_buffer_bytes,
        );
    }
    // 3. config: surface unknown-key warnings (they never abort — D14).
    for w in &warns {
        log::warn!("[config] unknown key: {w}");
    }
    // 4. instance lock on the DATA ROOT, taken BEFORE any port bind (§8.3): it,
    //    not the port, protects the single-device_uuid / single-session.db
    //    invariants. A second daemon on the same root is diagnosed → exit 3.
    let _lock = InstanceLock::acquire(&roots.data).map_err(diagnose_lock)?;
    // 5. port bind + foreign-occupant diagnosis — STATELESS, so it runs BEFORE
    //    stores (6) and runtime composition (7) per the §8.1 order. On a bind
    //    conflict the occupant is probed with GET /api/ping: a qbzd answer means
    //    a stale foreign root (the lock said this root was free), anything else
    //    the §2.2 "another process" copy. The socket is bound here but not served
    //    until step 11 — connections queue in the listen backlog through boot.
    let bind_addr = resolve_bind_addr(&cfg)?;
    let bound = match crate::api::bind(bind_addr) {
        Ok(b) => b,
        Err(crate::api::BindError::AddrInUse(addr)) => return Err(diagnose_port_conflict(addr)),
        Err(crate::api::BindError::Other(msg)) => {
            return Err(format!(
                "error: could not bind the control API on {bind_addr}: {msg}\n  → check [server] bind/port in ~/.config/qbzd/qbzd.toml"
            ));
        }
    };
    if !bind_addr.ip().is_loopback() {
        // FB6: the default bind is now 0.0.0.0 — LAN-first posture (Sonos/
        // Chromecast parity), not a misconfiguration. One INFO line, not a
        // stderr warning; loopback binds stay silent.
        let token_set = cfg
            .server
            .token
            .as_deref()
            .is_some_and(|t| !t.trim().is_empty());
        log::info!(
            "{}",
            crate::cli::copy::lan_posture_note(&bind_addr.to_string(), token_set)
        );
    }

    // 6.-9. compose stores + runtime + restore credentials + restore session.
    let mut booted = boot(&roots, &cfg, warns.len()).await?;

    // 9b. Memory-pressure watchdog (issue #660): 15 s loop that evicts the
    //     L1 audio cache and halts prefetch under critical pressure. Holds an
    //     Arc<Player> (not Arc<AppRuntime>), so it sits outside the #521/§8.2
    //     ordering — aborted alongside the scrobbler for a clean shutdown.
    let memwatch = qbz_app::memory_watchdog::spawn(booted.runtime.core().player());

    // Attach the CoreEvent bus to the shared state so the qconnect lifecycle
    // latches can publish `QconnectSessionChanged` (SSE + the event hook).
    if let Ok(mut s) = booted.shared.lock() {
        s.bus = Some(booted.bus.clone());
    }

    // 10. playback driver (T4). Spawn the 450 ms headless orchestrator on the
    //     booted runtime + shared state. It runs safely regardless of auth: with
    //     no session the queue is empty and each tick is a near-no-op. The
    //     streaming quality is resolved from daemon_prefs through the SAME key
    //     contract the desktop uses (playback_quality(), playback.rs:170-172),
    //     so hi-res never silently downgrades. 11. HTTP serve (T6) · 12. QConnect
    //     (T9/T10) splice after this, reading `booted`.
    let prefs = daemon_prefs::load_at(&roots.data);
    let quality = playback_driver::quality_from_key(&prefs.streaming_quality);
    let qconnect_authority = Arc::new(crate::qconnect::authority::AuthorityCell::new());
    // Owner-only playback consumers use a parallel, daemon-local bus whose
    // events carry the exact authority observation made by events_bridge.
    // Keep the public CoreEvent bus unchanged for SSE/MPRIS/hooks.
    let (owner_event_tx, _) =
        broadcast::channel::<crate::events_bridge::AuthorityStampedEvent>(256);
    // T11: a live-updatable cell, not a value captured once — `settings/reload`
    // re-reads `daemon_prefs` and writes here so the driver's OWN auto-advance
    // (gapless prefetch, natural-end advance) picks up a `playback.quality`
    // change without a restart. Manual play/next/prev already re-read
    // `daemon_prefs` fresh every call (api/playback.rs::resolve_quality); this
    // cell is what makes the BACKGROUND driver loop equally live.
    let quality_cell = Arc::new(std::sync::Mutex::new(quality));
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    // T10 (§7.2): the driver's `ReportEdge` action pulses this Notify; the
    // QConnect report scheduler (step 12) waits on it. Created BEFORE the driver
    // so `on_edge` can capture it, and shared with `qconnect::start`.
    let report_notify = Arc::new(tokio::sync::Notify::new());
    // The same edge, pulsed onto a SECOND Notify for the events bridge (10e):
    // two subscribers on one Notify would race for the single stored permit.
    let edge_notify = Arc::new(tokio::sync::Notify::new());
    let deps = build_driver_deps(
        quality_cell.clone(),
        booted.shared.clone(),
        report_notify.clone(),
        edge_notify.clone(),
        booted.bus.clone(),
        qconnect_authority.clone(),
    );
    let driver = tokio::spawn(playback_driver::run_driver(
        booted.runtime.clone(),
        deps,
        shutdown_rx,
    ));

    // 10b. Queue-persistence subscriber (T10, §7.5): a `CoreEvent::QueueUpdated`
    //      on the DaemonAdapter bus — from driver auto-advance, the CLI queue
    //      verbs OR a QConnect-driven remote mutation — is debounced 2 s and then
    //      flushed to the session store, so a restart resumes the remote-set queue
    //      PAUSED (boot already restores it, §8.1-9½). Holds an `Arc<AppRuntime>`
    //      clone, so it is aborted+joined ahead of `drop(booted)` (#521 ordering).
    let queue_persist = spawn_queue_persist(
        booted.runtime.clone(),
        booted.bus.subscribe(),
        qconnect_authority.clone(),
    );

    // 10c. Scrobble-on-play (CONSOLE): an authority-stamped event subscriber that sends
    //      "now playing" on TrackStarted and scrobbles once past the Last.fm
    //      threshold, to whichever of Last.fm / ListenBrainz is connected +
    //      enabled in the scrobbler store. Holds NO Arc<AppRuntime>, so it sits
    //      outside the #521/§8.2 ordering — aborted for a clean shutdown below.
    let scrobbler = crate::scrobble_engine::spawn(
        roots.clone(),
        owner_event_tx.subscribe(),
        qconnect_authority.clone(),
    );

    // 10c¼. Listen log (§12.1): the scrobbler's sibling on the same stamped bus,
    //       WITHOUT its enabled gate — the log is local and on by default, so
    //       a headless streamer finally contributes history. Holds no
    //       Arc<AppRuntime>; stopped (abort + close the open row) below.
    let listen_log = crate::listen_log_engine::spawn(
        &roots,
        owner_event_tx.subscribe(),
        qconnect_authority.clone(),
    )
    .await;

    // 10c½. Events bridge: translate the driver's transition edges into the
    //       playback CoreEvents the bus consumers above (and SSE, and the event
    //       hook below) are written for — TrackStarted / PlaybackStateChanged /
    //       PositionUpdated / VolumeChanged. Holds only a Weak<AppRuntime>
    //       upgraded per wake, but is still aborted+joined ahead of
    //       `drop(booted)` so a mid-wake strong Arc can't outlive the ordering.
    let events_bridge = crate::events_bridge::spawn(
        &booted.runtime,
        booted.bus.clone(),
        owner_event_tx,
        edge_notify,
        qconnect_authority.clone(),
    );

    // 10c¾. Event hook (CONSOLE): fork `hooks.script` (or `QBZD_HOOK`) once per
    //       forwarded bus event with QBZ_* variables in its environment — push
    //       integration for headless boxes (moOde-style renderer coordination).
    //       Holds no Arc<AppRuntime>; aborted for a clean shutdown.
    let hooks = match crate::hooks::script(&roots) {
        Some(path) => {
            log::info!("[hooks] running {} on daemon events (hooks.script)", path.display());
            Some(crate::hooks::spawn(path, booted.bus.subscribe()))
        }
        None => None,
    };

    // 10d. MPRIS media controls (CONSOLE): publish org.mpris.MediaPlayer2 so a
    //      KDE/GNOME media widget, a plasmoid, or hardware media keys drive the
    //      daemon with no custom client. The inbound callback holds a
    //      Weak<AppRuntime> (never pins the runtime), so it too sits outside the
    //      #521 ordering; None on a headless box / when QBZD_MPRIS disables it.
    let mpris = crate::mpris::spawn(
        &booted.runtime,
        roots.clone(),
        booted.bus.subscribe(),
        tokio::runtime::Handle::current(),
        qconnect_authority.clone(),
    );

    // 11. HTTP serve (02 §3) on the already-bound socket. `ApiState` carries a
    //     second read-only audio-store connection (WAL) for the status audio
    //     block, the tokio handle for the async queue read, and the opt-in
    //     [server] token (None = open). 12. QConnect (T9/T10) splices after this.
    let api_audio = qbz_audio::settings::AudioSettingsStore::new_at(&roots.data)
        .map_err(|e| format!("error: could not open the audio settings store for the API: {e}"))?;
    // T11: the reload handler's "did a routing-critical field change" diff
    // needs a starting point — seed it from what's on disk right now (the same
    // settings the Player was constructed with at step 6/7).
    let initial_audio_settings = api_audio.get_settings().unwrap_or_default();
    // T11: the running QConnect service is not constructed until step 12
    // (AFTER the API starts serving, per the normative boot order, 01 §8.1) —
    // this cell lets the reload route reach it anyway: empty until `qconnect
    // ::start` below populates it, harmlessly no-op'd by the reload handler in
    // the vanishingly small window before that.
    let qconnect_control: Arc<std::sync::OnceLock<crate::qconnect::QconnectControl>> =
        Arc::new(std::sync::OnceLock::new());
    // Orbit uses the daemon's OWN roots. Construction is inert and local
    // library access never depends on Qobuz authentication. No second port.
    let library = orbit.then(|| Arc::new(qbz_library::service::LibraryService::new(
        qbz_library::LibraryStore::new(roots.data.join("library.db")),
        roots.cache.join("artwork"),
    )));
    let library_endpoint = library.as_ref().map(|service| qbz_control::library::LibraryEndpoint::new(
        service.clone(), std::env::var("HOSTNAME").ok().filter(|s| !s.trim().is_empty()).unwrap_or_else(|| "qbzd".into()), true,
    ));
    let api = crate::api::serve(
        bound,
        crate::api::ApiState {
            library: library_endpoint,
            runtime: booted.runtime.clone(),
            shared: booted.shared.clone(),
            bus: booted.bus.clone(),
            roots: roots.clone(),
            token: cfg.server.token.filter(|t| !t.trim().is_empty()),
            bind: bind_addr.to_string(),
            rt: tokio::runtime::Handle::current(),
            audio: api_audio,
            devices: std::sync::Mutex::new(crate::api::DeviceCache::default()),
            audio_snapshot: std::sync::Mutex::new(initial_audio_settings),
            quality: quality_cell.clone(),
            qconnect_control: qconnect_control.clone(),
        },
    );
    log::info!("control API listening on {bind_addr}");

    // 12. QConnect (T9): mint the daemon's OWN device identity in the daemon-root
    //     KV, decide auto-connect from the persisted startup mode (cli_override =
    //     None so the KV that `qbzd qconnect enable|disable` writes is never
    //     shadowed), and — when enabled — connect-on-Ready with the bounded retry
    //     schedule. Reads NOTHING from qbzd.toml. Held to shut the session down
    //     ahead of playback (§8.2-1); it also clones `Arc<AppRuntime>`, so it must
    //     drop before `drop(booted)` (the #521 ordering).
    let mut qconnect = crate::qconnect::start(
        booted.runtime.clone(),
        booted.shared.clone(),
        qconnect_authority.clone(),
        &roots,
        quality_cell.clone(),
        report_notify,
        booted.bus.subscribe(),
    );
    // T11: publish the reload route's handle onto the running service now that
    // it exists (`connect`/`disconnect`/device-name refresh — see
    // `qconnect::QconnectControl`).
    let _ = qconnect_control.set(qconnect.control());

    // 13. park on SIGTERM/SIGINT. NO startup audio "hygiene": both candidate
    //     fns are verified no-ops from a fresh process and re-adding them is the
    //     documented skeptic-correction #1 trap (§8.1).
    wait_for_signal().await;
    // Reject new library work immediately; join after the API stops serving.
    if let Some(library) = &library { library.close(); }

    // ── Shutdown (§8.2, ordered). Step 1: disconnect the QConnect session (and
    //    stop its auto-connect watcher) BEFORE playback is stopped, then drop the
    //    handle so its Arc<AppRuntime> clone is released ahead of `drop(booted)`.
    qconnect.shutdown().await;
    drop(qconnect);
    // Step 2: stop the playback driver. It holds an Arc<AppRuntime> clone, so its
    //    task must finish (dropping that Arc) before `drop(booted)` can release
    //    the audio device ahead of the #521 pair. Signal, then join.
    let _ = shutdown_tx.send(true);
    if let Err(e) = driver.await {
        log::warn!("driver task join failed: {e:?}");
    }
    // T10 (§7.5): stop the queue-persistence subscriber before the authoritative
    // final save, so it neither races the flush below nor keeps its
    // `Arc<AppRuntime>` clone alive past `drop(booted)` (#521 ordering).
    queue_persist.abort();
    let _ = queue_persist.await;
    // Stop the scrobble-on-play subscriber (holds no Arc<AppRuntime>; order-free).
    scrobbler.abort();
    let _ = scrobbler.await;
    // Stop the listen log: abort, then close the row in flight as `shutdown`.
    listen_log.stop().await;
    // Stop the memory-pressure watchdog and JOIN it: it holds an Arc<Player>
    // clone, which must drop before `drop(booted)` can release the audio
    // device — the same ordering constraint as the driver (§8.2).
    memwatch.abort();
    let _ = memwatch.await;
    // Stop the events bridge BEFORE `drop(booted)`: it upgrades its Weak to a
    // strong Arc<AppRuntime> for the span of each wake (#521 ordering).
    events_bridge.abort();
    let _ = events_bridge.await;
    // Stop the event-hook dispatcher (holds no Arc<AppRuntime>; order-free).
    if let Some(hooks) = hooks {
        hooks.abort();
        let _ = hooks.await;
    }
    // Tear down MPRIS: abort its updater and drop the D-Bus handle. Its inbound
    // callback held only a Weak<AppRuntime>, so this is order-free too.
    if let Some(mpris) = mpris {
        mpris.shutdown().await;
    }
    // Final full session save (queue + position) now that playback is quiesced.
    playback_driver::save_session_now(booted.runtime.as_ref()).await;
    // The background auth-retry task also holds an Arc<AppRuntime> clone — abort
    // AND join it so its Arc is dropped before `drop(booted)`; otherwise the
    // ordering claim below (drop releases the device) breaks once playback has
    // engaged a real device.
    if let Some(retry) = booted.auth_retry.take() {
        retry.abort();
        let _ = retry.await;
    }
    // Stop the API thread and JOIN it: its `ApiState` holds an `Arc<AppRuntime>`
    // clone, which must drop before `drop(booted)` releases the audio device
    // ahead of the #521 pair — the same ordering constraint as the driver and
    // auth-retry tasks (§8.2).
    api.shutdown();
    if let Some(library) = library {
        let _ = tokio::task::spawn_blocking(move || library.shutdown()).await;
    }
    // The reload route's OnceLock handle also clones `QconnectControl`, which
    // holds an `Arc<AppRuntime>` (via `DaemonQconnectService.runtime`) — drop
    // it before `drop(booted)` too, same #521/§8.2 ordering as the driver,
    // queue-persist and auth-retry tasks above.
    drop(qconnect_control);
    // Release the audio device by dropping the runtime (its Player) BEFORE the
    // #521 pair (§8.2 step 3 precedes step 4).
    drop(booted);
    //    THE #521 PAIR runs unconditionally on Linux — exactly the desktop quit
    //    choke-point (crates/qbz/src/main.rs:20393): a forced PipeWire clock left
    //    set would pin the whole system's sample rate after the process dies.
    //    Both calls self-gate to no-ops when QBZ forced nothing.
    #[cfg(target_os = "linux")]
    {
        let _ = qbz_audio::alsa_backend::resume_suspended_sink();
        qbz_audio::pipewire_backend::PipeWireBackend::reset_pipewire_clock();
    }

    Ok(0) // instance lock released on drop of `_lock`
}

/// Steps 6-9 of §8.1: open the daemon-root stores, compose the runtime with the
/// two NORMATIVE substitutions (`with_audio_settings` + `activate_at`), and
/// restore the saved session per the §6.2 clearing taxonomy.
fn revalidate_alsa_hardware_volume(
    store: &qbz_audio::settings::AudioSettingsStore,
) -> Result<(qbz_audio::settings::AudioSettings, Option<f32>), String> {
    use qbz_audio::alsa_hardware_volume::{
        HardwareVolumeDecision, HardwareVolumeSelectionSource, HardwareVolumeUnsupportedReason,
    };

    let audio = store.get_settings()?;
    if !audio.alsa_hardware_volume {
        return Ok((audio, None));
    }
    if !qbz_audio::alsa_direct::uses_alsa_direct_route(&audio) {
        log::warn!(
            "[ALSA HW Volume] enabled outside ALSA Direct; disabling without touching a mixer"
        );
        store.set_alsa_hardware_volume(false)?;
        return Ok((store.get_settings()?, None));
    }
    let device_id = audio
        .output_device
        .as_deref()
        .expect("ALSA Direct route has a device id");
    let probe = qbz_audio::alsa_hardware_volume::enumerate_hardware_volume_controls(device_id);
    let decision = qbz_audio::alsa_hardware_volume::decide_hardware_volume(
        &probe,
        &audio.alsa_hardware_volume_controls,
    );
    match decision {
        HardwareVolumeDecision::Selected { selection } => {
            let Some(volume) = selection.candidate.current_volume else {
                log::warn!(
                    "[ALSA HW Volume] selected control '{}' has no readable level; disabling",
                    selection.control
                );
                store.set_alsa_hardware_volume(false)?;
                return Ok((store.get_settings()?, None));
            };
            if selection.source != HardwareVolumeSelectionSource::Persisted {
                // Backward-compatible migration for an old bare boolean. The
                // probe is read-only; persist only an unequivocal identity.
                store.set_alsa_hardware_volume_control(
                    selection.route_key,
                    selection.control.clone(),
                )?;
            }
            log::info!(
                "[ALSA HW Volume] validated '{}' at {:.0}%",
                selection.control,
                volume * 100.0
            );
            Ok((store.get_settings()?, Some(volume)))
        }
        HardwareVolumeDecision::NeedsChoice { reason, .. } => {
            log::warn!(
                "[ALSA HW Volume] explicit mixer choice required ({reason:?}); disabling without writing"
            );
            store.set_alsa_hardware_volume(false)?;
            Ok((store.get_settings()?, None))
        }
        HardwareVolumeDecision::Unsupported { reason, .. } => {
            let detail = match &reason {
                HardwareVolumeUnsupportedReason::Probe(error) => error.message.clone(),
                other => format!("{other:?}"),
            };
            log::warn!(
                "[ALSA HW Volume] saved mixer control unavailable ({detail}); disabling without fallback"
            );
            store.set_alsa_hardware_volume(false)?;
            Ok((store.get_settings()?, None))
        }
    }
}

async fn boot(
    roots: &ProfileRoots,
    cfg: &QbzdConfig,
    warn_count: usize,
) -> Result<BootedRuntime, String> {
    // 6.+7. stores + runtime composition. The two substitutions (01 §2.2):
    //   - with_audio_settings, NOT AppRuntime::new (which hardcodes the
    //     desktop-global AudioSettingsStore — shell.rs:87-101);
    //   - activate_at (below), NOT activate (which resolves desktop
    //     UserDataPaths — shell.rs:195-203).
    // Everything routes through the T2 daemon roots.
    let store = qbz_audio::settings::AudioSettingsStore::new_at(&roots.data)?; // settings.rs:263
    let (settings, hardware_volume) = revalidate_alsa_hardware_volume(&store)?;
    let (adapter, _rx) = DaemonAdapter::new();
    let bus = adapter.sender();
    let runtime = Arc::new(AppRuntime::with_audio_settings(
        adapter,
        settings.output_device.clone(),
        settings,
        None,
    )); // shell.rs:64
    if let Some(volume) = hardware_volume {
        runtime.core().player().seed_volume_state(volume);
    }

    // Offline-tolerant (§8.1-8): a network failure here still leaves a locally
    // usable core; a missing DAC is likewise non-fatal (Player starts deviceless
    // and retries with backoff — never the spotifyd #1097 crash-exit).
    if let Err(e) = runtime.init().await {
        log::warn!("core init did not complete (continuing offline-tolerant): {e}");
    }

    // Playlist recommendations (CONSOLE): open the per-user artist-vector store
    // at the DAEMON root — mirrors qbz/src/auth.rs:145-149, but slint-free and
    // session-independent. The store is a CACHE the suggestions engine reads/
    // writes; vectors are built on demand from MusicBrainz + Qobuz, so this
    // needs no listening history. NOT optional for the feature, though: with
    // `artist_vectors = None` the engine does not run un-cached — both
    // `suggestions.rs` and `builder.rs` answer "No active session" — so a
    // failed open is logged loudly instead of leaving a silent, dead route.
    match qbz_reco::ArtistVectorStore::open_at(&roots.data) {
        Ok(store) => runtime.core().set_artist_vectors(store).await,
        Err(e) => log::warn!(
            "artist-vector store open failed ({e}); playlist suggestions answer \"No active session\" until it opens"
        ),
    }

    let shared = new_shared(cfg);
    if let Ok(mut s) = shared.lock() {
        s.startup_warnings = warn_count as u32;
    }

    // 8. credential restore per the §6.2 taxonomy (mirrors qbz/src/auth.rs:
    //    215-230): clear the token ONLY on explicit auth rejection; KEEP it on
    //    every network-class failure (clearing on transient errors is the
    //    documented boot-token-loss bug class).
    let auth_retry = match qbz_credentials::load_oauth_token_at(&roots.config)? {
        None => {
            set_needs_auth(&shared, None);
            // `None` covers both "no token saved" and "token saved but this
            // process cannot decrypt it" — the decrypt failure is swallowed by
            // design so a broken file can never abort boot. Tell them apart
            // here, or `status` reports "not logged in / last error: none" and
            // the real cause stays buried in the log.
            if qbz_credentials::oauth_token_file_present_at(&roots.config) {
                latch_undecryptable_token(&shared);
            }
            None
        }
        Some(token) => {
            // Register before the token can reach any log line (§6.3).
            qbz_log::register_secret(token.clone());
            match runtime.core().login_with_token(&token).await {
                Ok(session) => {
                    restore_activate(&runtime, &shared, roots, session, &token).await?;
                    // 9½. session restore (queue/position) PAUSED: the daemon's
                    //     session store IS its queue persistence, so a restart
                    //     comes back with the queue armed but not auto-playing.
                    playback_driver::restore_session_paused(runtime.as_ref()).await;
                    None
                }
                Err(e) if is_auth_rejection(&e) => {
                    qbz_credentials::clear_oauth_token_at(&roots.config)?;
                    latch_auth_error(&shared, &e);
                    set_needs_auth(&shared, Some(e));
                    None
                }
                Err(e) => {
                    // network-class: KEEP token, stay Restoring, retry w/ backoff.
                    log::warn!("session restore deferred (network-class): {e}");
                    // 01 §9.3: a real network-class outcome — latch `network.online`
                    // false so `/api/status` reflects it until a retry succeeds.
                    if let Ok(s) = shared.lock() {
                        s.set_network_online(false);
                    }
                    Some(spawn_auth_retry(
                        runtime.clone(),
                        shared.clone(),
                        roots.clone(),
                    ))
                }
            }
        }
    };

    Ok(BootedRuntime {
        runtime,
        shared,
        bus,
        auth_retry,
    })
}

/// Assemble the driver's host side channels: the streaming-quality resolver and
/// the daemon-shared latching / tick-timestamping hooks. T10: `on_edge` now
/// pulses the QConnect report `Notify` so the report scheduler reports on the
/// same transition/periodic edges the driver detects (§7.2) — and, on a second
/// `Notify`, the events bridge (10c½) that publishes those edges to the bus.
fn build_driver_deps(
    quality_cell: Arc<std::sync::Mutex<qbz_models::Quality>>,
    shared: Arc<Mutex<DaemonShared>>,
    report_notify: Arc<tokio::sync::Notify>,
    edge_notify: Arc<tokio::sync::Notify>,
    bus: tokio::sync::broadcast::Sender<qbz_models::CoreEvent>,
    qconnect_authority: Arc<crate::qconnect::authority::AuthorityCell>,
) -> DriverDeps {
    let latch_shared = shared.clone();
    let tick_shared = shared;
    let qconnect_authority_allowed = Arc::clone(&qconnect_authority);
    let qconnect_authority_observe = Arc::clone(&qconnect_authority);
    let qconnect_authority_readmit = qconnect_authority;
    DriverDeps {
        quality: Arc::new(move || {
            quality_cell
                .lock()
                .map(|q| *q)
                .unwrap_or(qbz_models::Quality::UltraHiRes)
        }),
        owner_actions_allowed: Arc::new(move || qconnect_authority_allowed.owner_actions_allowed()),
        observe_authority: Arc::new(move || {
            match qconnect_authority_observe.observe_owner_authority() {
                crate::qconnect::authority::OwnerAuthorityObservation::Owner {
                    token,
                    permit,
                } => qbz_app::playback_driver::DriverAuthorityObservation::Owner(
                    qbz_app::playback_driver::DriverOwnerAdmission::new(
                        qbz_app::playback_driver::DriverOwnerToken::new(token),
                        qbz_app::playback_driver::DriverActionPermit::new(permit),
                    ),
                ),
                crate::qconnect::authority::OwnerAuthorityObservation::Delegated => {
                    qbz_app::playback_driver::DriverAuthorityObservation::Delegated
                }
                crate::qconnect::authority::OwnerAuthorityObservation::Fenced => {
                    qbz_app::playback_driver::DriverAuthorityObservation::Fenced
                }
            }
        }),
        readmit_owner_action: Arc::new(move |token| {
            let token = token.downcast_ref::<crate::qconnect::authority::OwnerAuthorityToken>()?;
            qconnect_authority_readmit
                .try_owner_action_permit_exact(*token)
                .map(qbz_app::playback_driver::DriverActionPermit::new)
        }),
        // T10: signal the report scheduler on every ReportEdge. `notify_one`
        // stores a single permit if the scheduler is mid-report, so no edge is
        // lost and rapid edges coalesce into one report.
        on_edge: Arc::new(move || {
            report_notify.notify_one();
            edge_notify.notify_one();
        }),
        on_latch: Arc::new(move |category, message| {
            if let Ok(mut s) = latch_shared.lock() {
                match category {
                    "stream" => s.last_errors.stream = Some(message.clone()),
                    "transport" => s.last_errors.transport = Some(message.clone()),
                    "auth" => s.last_errors.auth = Some(message.clone()),
                    _ => {}
                }
            }
            // Surface stream failures on the bus too, so `/api/events` and the
            // event hook see them live (e.g. a busy ALSA device at play time)
            // instead of only the polled /api/status error latch.
            if category == "stream" {
                let _ = bus.send(qbz_models::CoreEvent::PlaybackError {
                    track_id: 0,
                    message,
                });
            }
        }),
        on_tick: Arc::new(move || {
            if let Ok(mut s) = tick_shared.lock() {
                s.driver_last_tick = Some(std::time::Instant::now());
            }
        }),
    }
}

/// T10 (§7.5): the queue-persistence subscriber. Debounces `CoreEvent::QueueUpdated`
/// bursts by 2 s, then flushes the live queue + position to the session store via
/// `save_session_now`. QConnect-driven mutations (`materialize_remote_queue` ->
/// `set_queue`) also emit `QueueUpdated`, so a remote-set queue survives a restart
/// (boot restores it PAUSED). Non-queue events (e.g. position ticks) are drained
/// WITHOUT extending the debounce window, so they can never starve the flush.
fn spawn_queue_persist(
    runtime: Arc<AppRuntime<DaemonAdapter>>,
    mut rx: broadcast::Receiver<CoreEvent>,
    qconnect_authority: Arc<crate::qconnect::authority::AuthorityCell>,
) -> JoinHandle<()> {
    use tokio::sync::broadcast::error::RecvError;
    const DEBOUNCE: std::time::Duration = std::time::Duration::from_secs(2);
    tokio::spawn(async move {
        loop {
            // Block until the FIRST queue mutation of a burst.
            match rx.recv().await {
                Ok(CoreEvent::QueueUpdated { .. }) => {}
                Ok(_) => continue,
                Err(RecvError::Lagged(_)) => continue,
                Err(RecvError::Closed) => return,
            }
            // Debounce: a fixed deadline that only a further QueueUpdated extends.
            // Other events are consumed but never push the deadline out.
            let mut deadline = tokio::time::Instant::now() + DEBOUNCE;
            loop {
                tokio::select! {
                    _ = tokio::time::sleep_until(deadline) => break,
                    r = rx.recv() => match r {
                        Ok(CoreEvent::QueueUpdated { .. }) => {
                            deadline = tokio::time::Instant::now() + DEBOUNCE;
                        }
                        Ok(_) => {}
                        Err(RecvError::Lagged(_)) => {}
                        Err(RecvError::Closed) => return,
                    }
                }
            }
            let Some(_owner_permit) = qconnect_authority.try_owner_action_permit() else {
                log::debug!("[qbzd] queue-persist: skipped delegated QConnect queue");
                continue;
            };
            playback_driver::save_session_now(runtime.as_ref()).await;
            log::debug!("[qbzd] queue-persist: session flushed after QueueUpdated burst");
        }
    })
}

/// Activate the per-user session against DAEMON paths (§8.1-9): inject the
/// session into the core, then `activate_at` the runtime with per-user daemon
/// data/cache directories — never the desktop `UserDataPaths`.
pub(crate) async fn restore_activate(
    runtime: &Arc<AppRuntime<DaemonAdapter>>,
    shared: &Arc<Mutex<DaemonShared>>,
    roots: &ProfileRoots,
    session: UserSession,
    token: &str,
) -> Result<(), String> {
    runtime
        .core()
        .set_session(session.clone())
        .await
        .map_err(|e| e.to_string())?;
    runtime
        .activate_at(
            session.user_id,
            &roots.data.join(format!("users/{}", session.user_id)),
            &roots.cache.join(format!("users/{}", session.user_id)),
        )
        .await?;
    set_logged_in(shared, &session);
    // T11: remember which token this activation applied so a later
    // `POST /api/settings/reload` can tell "same token" from "new token" and
    // skip a redundant re-login on every unrelated settings nudge.
    if let Ok(mut s) = shared.lock() {
        s.credential_fingerprint = Some(crate::state::token_fingerprint(token));
        // 01 §9.3: a real login/restore success (boot, background auth-retry,
        // or a reload's credential re-validation — every caller of this fn)
        // means the network is reachable — latch `network.online` back true.
        s.set_network_online(true);
    }
    Ok(())
}

/// Fresh shared state. Starts in `Restoring` — credential restore drives the
/// terminal transition to `LoggedIn` or `NeedsAuth` (§6.2 diagram).
fn new_shared(cfg: &QbzdConfig) -> Arc<Mutex<DaemonShared>> {
    let _ = cfg; // reserved: premute/mpris defaults wire in with later tasks.
    Arc::new(Mutex::new(DaemonShared {
        auth: AuthState::Restoring,
        user_id: None,
        subscription: None,
        last_errors: LatchedErrors::default(),
        driver_last_tick: None,
        muted: false,
        premute_volume: 1.0,
        started_at: std::time::Instant::now(),
        startup_warnings: 0,
        qconnect: QconnectStatus::default(),
        credential_fingerprint: None,
        network_online: std::sync::atomic::AtomicBool::new(true),
        // Attached by daemon::run right after boot (the bus outlives boot).
        bus: None,
    }))
}

/// Enter NeedsAuth. `err = None` = no saved credentials at all (the common
/// first-run case); `Some(e)` = an explicit auth rejection just cleared the
/// token. Either way the daemon STAYS UP (§6.2) and names the fix.
pub(crate) fn set_needs_auth(shared: &Arc<Mutex<DaemonShared>>, err: Option<CoreError>) {
    if let Ok(mut s) = shared.lock() {
        s.auth = AuthState::NeedsAuth;
        s.user_id = None;
        s.subscription = None;
        // T11: NeedsAuth has no applied token by definition.
        s.credential_fingerprint = None;
    }
    match err {
        None => log::info!("Not logged in — run 'qbzd setup' (or 'qbzd login')"),
        Some(e) => {
            log::warn!("Qobuz rejected the saved session ({e}) — run 'qbzd login' to re-authenticate")
        }
    }
}

/// Enter LoggedIn (Ready). Records the user id + subscription label for
/// `/api/status` (T6). The auth token itself is never stored here — it is a
/// registered secret and lives only in the credential file.
fn set_logged_in(shared: &Arc<Mutex<DaemonShared>>, session: &UserSession) {
    if let Ok(mut s) = shared.lock() {
        s.auth = AuthState::LoggedIn;
        s.user_id = Some(session.user_id);
        s.subscription = Some(session.subscription_label.clone());
    }
    log::info!(
        "Logged in (user {}, subscription '{}')",
        session.user_id,
        session.subscription_label
    );
}

/// Latch the "saved token is present but undecryptable" case. Reachable when a
/// token was written under a key this process cannot derive — e.g. a login that
/// ran in a graphical session against a build that still mixed the XDG portal
/// secret in, now read by an init-started daemon with no session bus. The
/// credential store migrates that token itself where it can (a daemon that CAN
/// reach the portal rewrites it portal-free); when it cannot, the only exit is a
/// fresh login, so say exactly that instead of a bare "not logged in".
pub(crate) fn latch_undecryptable_token(shared: &Arc<Mutex<DaemonShared>>) {
    if let Ok(mut s) = shared.lock() {
        s.last_errors.auth = Some(
            "the saved token could not be decrypted by this daemon — run 'qbzd login' to re-authenticate".into(),
        );
    }
    log::warn!(
        "saved token present but undecryptable — run 'qbzd login' to re-authenticate"
    );
}

/// Latch an auth error so a `status` call remains diagnosable after the fact
/// (§9.4 — drain-once channels alone cannot answer "why did the music stop?").
pub(crate) fn latch_auth_error(shared: &Arc<Mutex<DaemonShared>>, e: &CoreError) {
    if let Ok(mut s) = shared.lock() {
        s.last_errors.auth = Some(format!("token rejected by Qobuz — cleared ({e})"));
    }
}

/// True ONLY for an explicit auth rejection from Qobuz — a 401 on the token
/// login (`AuthenticationError`) or an ineligible-account verdict. Network
/// failures, offline gate, 5xx, rate limiting and parse errors all return false
/// so the saved token is KEPT (mirrors crates/qbz/src/auth.rs:215-230; the
/// taxonomy — not the variant list — is the normative part).
pub(crate) fn is_auth_rejection(error: &CoreError) -> bool {
    matches!(
        error,
        CoreError::Api(
            qbz_qobuz::ApiError::AuthenticationError(_) | qbz_qobuz::ApiError::IneligibleUser
        )
    )
}

/// Background retry for a network-class restore failure (§6.2: stay in the
/// authenticating state, KEEP the token, retry with backoff). On success the
/// session activates; on a now-explicit auth rejection the token is cleared and
/// the daemon drops to NeedsAuth; if the whole schedule sees only network-class
/// failures the token is KEPT and the daemon surfaces NeedsAuth so it stays
/// diagnosable and a later `qbzd login` / settings reload can retry.
fn spawn_auth_retry(
    runtime: Arc<AppRuntime<DaemonAdapter>>,
    shared: Arc<Mutex<DaemonShared>>,
    roots: ProfileRoots,
) -> JoinHandle<()> {
    const SCHEDULE_SECS: [u64; 4] = [2, 5, 15, 30];
    tokio::spawn(async move {
        let token = match qbz_credentials::load_oauth_token_at(&roots.config) {
            Ok(Some(t)) => t,
            _ => return, // token vanished (concurrent logout) — nothing to retry.
        };
        for (i, delay) in SCHEDULE_SECS.iter().enumerate() {
            tokio::time::sleep(std::time::Duration::from_secs(*delay)).await;
            log::info!("session restore retry {}/{}", i + 1, SCHEDULE_SECS.len());
            match runtime.core().login_with_token(&token).await {
                Ok(session) => {
                    if let Err(e) =
                        restore_activate(&runtime, &shared, &roots, session, &token).await
                    {
                        log::warn!("session activation after retry failed: {e}");
                    }
                    return;
                }
                Err(e) if is_auth_rejection(&e) => {
                    let _ = qbz_credentials::clear_oauth_token_at(&roots.config);
                    latch_auth_error(&shared, &e);
                    set_needs_auth(&shared, Some(e));
                    return;
                }
                Err(e) => {
                    log::warn!("session restore retry {} failed (network-class): {e}", i + 1);
                    // 01 §9.3: latch `network.online` false on every real
                    // network-class outcome, not just the first.
                    if let Ok(s) = shared.lock() {
                        s.set_network_online(false);
                    }
                }
            }
        }
        // Schedule exhausted with only network-class failures: KEEP the token,
        // surface NeedsAuth, latch the reason for `qbzd status`.
        if let Ok(mut s) = shared.lock() {
            s.auth = AuthState::NeedsAuth;
            s.set_network_online(false);
            s.last_errors.auth = Some(
                "could not reach Qobuz to restore the saved session — token kept, retry with 'qbzd login' or 'qbzd settings reload'".into(),
            );
        }
        log::warn!(
            "session restore gave up after {} network-class attempts — token KEPT",
            SCHEDULE_SECS.len()
        );
    })
}

/// Resolve `[server] bind:port` to a `SocketAddr` (01 §10.1). A malformed value
/// is a fatal boot error that names the fix (exit 1).
fn resolve_bind_addr(cfg: &QbzdConfig) -> Result<std::net::SocketAddr, String> {
    use std::net::ToSocketAddrs;
    let hostport = format!("{}:{}", cfg.server.bind, cfg.server.port);
    hostport
        .to_socket_addrs()
        .map_err(|e| {
            format!("error: invalid [server] bind/port '{hostport}': {e}\n  → set a valid ip and port in ~/.config/qbzd/qbzd.toml")
        })?
        .next()
        .ok_or_else(|| {
            format!("error: [server] '{hostport}' resolved to no address\n  → set a valid ip and port in ~/.config/qbzd/qbzd.toml")
        })
}

/// The step-5 bind-conflict diagnosis (02 §8.1-5 / §2.2): a qbzd occupant on a
/// different data root vs. an unrelated process on the port.
fn diagnose_port_conflict(addr: std::net::SocketAddr) -> String {
    if crate::api::probe_is_qbzd(addr) {
        crate::cli::copy::foreign_qbzd(&addr.to_string())
    } else {
        crate::cli::copy::port_in_use(addr.port())
    }
}

/// Render an [`InstanceLock`] failure. For the already-running case this prints
/// the frozen exit-3 error voice (02 §1.3/§1.4) and exits 3 directly — the new
/// process must never clobber the running one. An I/O failure returns a String
/// that propagates to a generic exit 1.
fn diagnose_lock(e: LockError) -> String {
    match e {
        LockError::AlreadyRunning(pid) => {
            let who = pid
                .map(|p| format!("(pid {p})"))
                .unwrap_or_else(|| "(pid unknown)".to_string());
            eprintln!("error: qbzd is already running {who}");
            eprintln!("  → stop it first:  systemctl --user stop qbzd");
            eprintln!("  → or inspect it:  systemctl --user status qbzd");
            log::logger().flush();
            std::process::exit(3);
        }
        LockError::Io(msg) => {
            format!("error: could not take the instance lock: {msg}\n  → check permissions on the data root")
        }
    }
}

/// Park until SIGTERM or SIGINT. A second signal after this returns lets the
/// default handler take over → immediate exit (§8.2).
async fn wait_for_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        match (
            signal(SignalKind::terminate()),
            signal(SignalKind::interrupt()),
        ) {
            (Ok(mut term), Ok(mut int)) => {
                tokio::select! {
                    _ = term.recv() => log::info!("SIGTERM received — shutting down"),
                    _ = int.recv()  => log::info!("SIGINT received — shutting down"),
                }
            }
            _ => {
                // Fall back to Ctrl-C if the SIGTERM handler could not install.
                let _ = tokio::signal::ctrl_c().await;
                log::info!("Ctrl-C received — shutting down");
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
        log::info!("Ctrl-C received — shutting down");
    }
}

// ============================ T11: settings reload ============================
// `POST /api/settings/reload` (02-cli-and-api.md §3.3.17; `crate::api::settings
// ::reload` is the thin HTTP wrapper) re-reads every engine store and applies
// what changed: audio (routing-critical -> `Player::reinit_device`, the rest ->
// `Player::reload_settings`), the daemon's own streaming-quality cell (the
// driver's background auto-advance), the QConnect KV (device-name cache +
// connect/disconnect reconciliation), and finally the credential file (absent
// -> NeedsAuth; new -> session restore). Never re-reads `qbzd.toml` (§3.1.2 —
// process config is boot-only). Response = the post-reload `/api/status` body,
// composed by the caller — zero new shapes (03-setup-tui.md §4.3: the
// reinit/reload narrative is composed CLIENT-side from the CLI's own copy of
// the Apply-ladder classification, never carried on the wire).

/// The single entry point the HTTP route calls. Audio/quality are independent;
/// credentials are reconciled next so their branch can retire LAN/delegated
/// QConnect before mutating owner auth, and QConnect KV reconciliation runs last
/// so an enabled service reconnects only against the settled credential state.
pub(crate) async fn reload(state: &crate::api::ApiState) {
    reload_audio(state);
    reload_quality(state);
    reload_credentials(
        &state.runtime,
        &state.shared,
        &state.roots,
        state.qconnect_control.get(),
    )
    .await;
    reload_qconnect(state).await;
}

/// Re-read `audio_settings.db` and apply it to the live `Player`. A struct
/// refresh (`reload_settings`) always happens; the output device is ADDITIONALLY
/// reinitialized only when a routing-critical field actually changed since the
/// last reload (mirrors the desktop's `Apply::Reinit` — `qbz/src/settings.rs:
/// 87-94`, per-key classification `:877-967,1134-1290`; 03-setup-tui.md §4.3
/// lists the same 9 fields).
pub(crate) fn reload_audio(state: &crate::api::ApiState) {
    // Audio reload is an owner-side live-player action. Keep this permit only
    // for the audio phase: `reload_credentials`/`reload_qconnect` below may
    // intentionally tear down delegated authority and must be free to acquire
    // and drain their own transition fence.
    let _owner_action = match state.qconnect_control.get() {
        None => None, // boot window: no delegated runtime can exist yet
        Some(qconnect) => {
            let Some(permit) = qconnect.try_owner_action_permit() else {
                log::info!(
                    "[reload] live audio settings not applied while QConnect owns playback; retry reload after owner authority returns"
                );
                return;
            };
            Some(permit)
        }
    };
    let (fresh, hardware_volume) = match revalidate_alsa_hardware_volume(&state.audio) {
        Ok(result) => result,
        Err(e) => {
            log::warn!("[reload] could not re-read audio settings: {e}");
            return;
        }
    };
    let player = state.runtime.core().player();
    if let Some(volume) = hardware_volume {
        // Seed before a routing-critical reinit so the new engine starts at
        // the physical level just read, never at synthetic unity.
        player.seed_volume_state(volume);
    }
    if let Err(e) = player.reload_settings(fresh.clone()) {
        log::warn!("[reload] player.reload_settings failed: {e}");
    }
    let needs_reinit = state
        .audio_snapshot
        .lock()
        .map(|old| audio_routing_changed(&old, &fresh))
        .unwrap_or(false);
    if needs_reinit {
        log::info!("[reload] routing-critical audio field changed — reinitializing the output device");
        if let Err(e) = player.reinit_device(fresh.output_device.clone()) {
            log::warn!("[reload] player.reinit_device failed: {e}");
        }
    }
    if let Ok(mut snap) = state.audio_snapshot.lock() {
        *snap = fresh;
    }
}

/// The Reinit-class field set (03-setup-tui.md §4.3 / `qbz/src/settings.rs:
/// 877-967,1134-1290`): backend, device, ALSA plugin, DSD mode, max sample
/// rate, exclusive mode, DAC passthrough, hardware volume, lock-output
/// (`skip_sink_switch`). Every other `AudioSettings` field is Reload-class —
/// `player.reload_settings` above already covers it unconditionally.
pub(crate) fn audio_routing_changed(
    old: &qbz_audio::settings::AudioSettings,
    new: &qbz_audio::settings::AudioSettings,
) -> bool {
    old.backend_type != new.backend_type
        || old.output_device != new.output_device
        || old.alsa_plugin != new.alsa_plugin
        || old.alsa_hardware_volume != new.alsa_hardware_volume
        || old.alsa_hardware_volume_controls != new.alsa_hardware_volume_controls
        || old.exclusive_mode != new.exclusive_mode
        || old.dac_passthrough != new.dac_passthrough
        || old.skip_sink_switch != new.skip_sink_switch
        || old.dsd_mode != new.dsd_mode
        || old.device_max_sample_rate != new.device_max_sample_rate
}

/// Re-read `daemon_prefs.streaming_quality` into the live cell the driver's
/// background auto-advance reads (`daemon.rs::run`'s `quality_cell`). Manual
/// play/next/prev already re-read `daemon_prefs` fresh every call
/// (`api/playback.rs::resolve_quality`); this is what makes the passive
/// natural-end-of-track advance equally live.
pub(crate) fn reload_quality(state: &crate::api::ApiState) {
    let prefs = daemon_prefs::load_at(&state.roots.data);
    let fresh = playback_driver::quality_from_key(&prefs.streaming_quality);
    if let Ok(mut q) = state.quality.lock() {
        *q = fresh;
    }
}

/// Re-cache the QConnect device-name override from the daemon-root KV (so the
/// NEXT connect uses whatever `qbzd qconnect name` / `settings set
/// qconnect.device_name` most recently wrote — 03-setup-tui.md §3.4: "applies
/// on the next connection", never forcing a reconnect just for a rename), then
/// reconcile the connect/disconnect state against the freshly-read
/// `startup_mode` (`qbzd qconnect enable|disable` — idempotent either way, see
/// `qconnect::QconnectControl`). A no-op before step 12 populates the cell.
pub(crate) async fn reload_qconnect(state: &crate::api::ApiState) {
    let Some(qc) = state.qconnect_control.get() else {
        return;
    };
    let db = state.roots.data.join("qconnect_settings.db");
    qc.refresh_device_name(&db).await;
    let mode = crate::qconnect::transport::load_startup_mode_at(&db);
    let should_connect = qconnect_app::compute_effective_startup(mode, None, None);
    if should_connect {
        if let Err(e) = qc.connect().await {
            log::info!("[reload] qconnect connect deferred: {e}");
        }
    } else if let Err(e) = qc.disconnect().await {
        log::warn!("[reload] qconnect disconnect failed: {e}");
    }
}

/// What the freshly-read credential file implies for the live session — pure
/// decision, unit-tested with no IO/network; [`reload_credentials`] just
/// executes whichever variant this returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CredentialAction {
    /// The file matches what's already applied (or both are absent/NeedsAuth
    /// already) — no network call, no state churn on an unrelated nudge.
    NoOp,
    /// The file is now empty but the daemon thinks it's logged in — tear the
    /// session down (mirrors what `qbzd logout` does to a running daemon,
    /// 02 §2.2: "QConnect session torn down, playback stopped").
    EnterNeedsAuth,
    /// A token is on disk that is not the one currently applied (fresh login
    /// out of NeedsAuth, a retry while Restoring, or an account switch) —
    /// validate and activate it.
    Apply(String),
}

pub(crate) fn decide_credential_action(
    current_auth: AuthState,
    current_fingerprint: Option<u64>,
    file_token: Option<String>,
) -> CredentialAction {
    match file_token {
        None => {
            if current_auth == AuthState::NeedsAuth {
                CredentialAction::NoOp
            } else {
                CredentialAction::EnterNeedsAuth
            }
        }
        Some(token) => {
            let fp = crate::state::token_fingerprint(&token);
            if current_auth == AuthState::LoggedIn && current_fingerprint == Some(fp) {
                CredentialAction::NoOp
            } else {
                CredentialAction::Apply(token)
            }
        }
    }
}

/// Re-read the credential file and reconcile the live session against it (02
/// §3.3.17: "absent → NeedsAuth transition; new → session restore"; taxonomy
/// shared with boot, §6.2). `qconnect` is `None` only in the brief boot window
/// before step 12 populates the cell — credential-change teardown is skipped
/// only then, because no LAN/coordinator/runtime exists yet.
pub(crate) async fn reload_credentials(
    runtime: &Arc<AppRuntime<DaemonAdapter>>,
    shared: &Arc<Mutex<DaemonShared>>,
    roots: &ProfileRoots,
    qconnect: Option<&crate::qconnect::QconnectControl>,
) {
    let file_token = match qbz_credentials::load_oauth_token_at(&roots.config) {
        Ok(t) => t,
        Err(e) => {
            log::warn!("[reload] could not read the credential file: {e}");
            return;
        }
    };
    let (current_auth, current_fp) = match shared.lock() {
        Ok(s) => (s.auth, s.credential_fingerprint),
        Err(_) => return,
    };

    match decide_credential_action(current_auth, current_fp, file_token) {
        CredentialAction::NoOp => {}
        CredentialAction::EnterNeedsAuth => {
            log::info!("[reload] credential file cleared — tearing the session down (NeedsAuth)");
            if let Some(qc) = qconnect {
                if let Err(e) = qc.disconnect_for_credentials_change().await {
                    // Fail closed: owner logout/deactivation must never race a
                    // surviving LAN candidate or delegated authority.
                    log::warn!(
                        "[reload] QConnect credential-change teardown failed; keeping current auth: {e}"
                    );
                    return;
                }
            }
            let _ = runtime.core().stop();
            let _ = runtime.core().logout().await;
            let _ = runtime.deactivate().await;
            set_needs_auth(shared, None);
        }
        CredentialAction::Apply(token) => {
            if let Some(qc) = qconnect {
                if let Err(e) = qc.disconnect_for_credentials_change().await {
                    // Do not install/account-switch the owner token until LAN,
                    // candidate validation, and delegated authority are gone.
                    log::warn!(
                        "[reload] QConnect credential-change teardown failed; token not applied: {e}"
                    );
                    return;
                }
            }
            qbz_log::register_secret(token.clone());
            match runtime.core().login_with_token(&token).await {
                Ok(session) => {
                    match restore_activate(runtime, shared, roots, session, &token).await {
                        Ok(()) => {
                            playback_driver::restore_session_paused(runtime.as_ref()).await;
                        }
                        Err(e) => log::warn!("[reload] session activation failed: {e}"),
                    }
                }
                Err(e) if is_auth_rejection(&e) => {
                    let _ = qbz_credentials::clear_oauth_token_at(&roots.config);
                    latch_auth_error(shared, &e);
                    set_needs_auth(shared, Some(e));
                }
                Err(e) => {
                    log::warn!("[reload] session restore deferred (network-class): {e}");
                    // 01 §9.3: real network-class outcome — latch it false.
                    if let Ok(s) = shared.lock() {
                        s.set_network_online(false);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_auth_rejection_matches_only_explicit_rejections() {
        // Explicit rejections → clear the token.
        assert!(is_auth_rejection(&CoreError::Api(
            qbz_qobuz::ApiError::AuthenticationError("401".into())
        )));
        assert!(is_auth_rejection(&CoreError::Api(
            qbz_qobuz::ApiError::IneligibleUser
        )));
        // Network-class / other → KEEP the token (the boot-token-loss guard).
        assert!(!is_auth_rejection(&CoreError::Api(
            qbz_qobuz::ApiError::ServerError(503)
        )));
        assert!(!is_auth_rejection(&CoreError::Api(
            qbz_qobuz::ApiError::RateLimited(30)
        )));
        assert!(!is_auth_rejection(&CoreError::NotInitialized));
    }

    #[test]
    fn no_credentials_enters_needs_auth() {
        let shared = new_shared(&QbzdConfig::default());
        set_needs_auth(&shared, None);
        let s = shared.lock().unwrap();
        assert_eq!(s.auth, AuthState::NeedsAuth);
        assert!(s.user_id.is_none());
        assert!(s.last_errors.auth.is_none());
    }

    #[test]
    fn explicit_rejection_latches_and_needs_auth() {
        let shared = new_shared(&QbzdConfig::default());
        let err = CoreError::Api(qbz_qobuz::ApiError::AuthenticationError("401".into()));
        latch_auth_error(&shared, &err);
        set_needs_auth(&shared, Some(err));
        let s = shared.lock().unwrap();
        assert_eq!(s.auth, AuthState::NeedsAuth);
        assert!(s.last_errors.auth.is_some());
    }

    #[test]
    fn logged_in_records_user_and_subscription() {
        let shared = new_shared(&QbzdConfig::default());
        let session = UserSession {
            user_auth_token: "secret".into(),
            user_id: 1234567,
            email: "a@b.c".into(),
            display_name: "Tester".into(),
            subscription_label: "studio".into(),
            ..UserSession::default()
        };
        set_logged_in(&shared, &session);
        let s = shared.lock().unwrap();
        assert_eq!(s.auth, AuthState::LoggedIn);
        assert_eq!(s.user_id, Some(1234567));
        assert_eq!(s.subscription.as_deref(), Some("studio"));
    }

    // ======================= T11: settings/reload =======================

    #[test]
    fn credential_action_noop_when_absent_and_already_needs_auth() {
        assert_eq!(
            decide_credential_action(AuthState::NeedsAuth, None, None),
            CredentialAction::NoOp
        );
    }

    #[test]
    fn credential_action_enters_needs_auth_when_file_cleared_while_logged_in() {
        // The `qbzd logout` case: file now absent, daemon still thinks it's
        // LoggedIn (or mid-Restoring) — must tear down.
        let fp = crate::state::token_fingerprint("old-token");
        assert_eq!(
            decide_credential_action(AuthState::LoggedIn, Some(fp), None),
            CredentialAction::EnterNeedsAuth
        );
        assert_eq!(
            decide_credential_action(AuthState::Restoring, None, None),
            CredentialAction::EnterNeedsAuth
        );
    }

    #[test]
    fn credential_action_noop_when_token_unchanged_and_logged_in() {
        let fp = crate::state::token_fingerprint("same-token");
        assert_eq!(
            decide_credential_action(AuthState::LoggedIn, Some(fp), Some("same-token".into())),
            CredentialAction::NoOp
        );
    }

    #[test]
    fn credential_action_applies_new_token_out_of_needs_auth() {
        assert_eq!(
            decide_credential_action(AuthState::NeedsAuth, None, Some("fresh-token".into())),
            CredentialAction::Apply("fresh-token".into())
        );
    }

    #[test]
    fn credential_action_applies_changed_token_while_already_logged_in() {
        // Account-switch / re-login case: fingerprint differs even though the
        // daemon is already LoggedIn.
        let old_fp = crate::state::token_fingerprint("old-token");
        assert_eq!(
            decide_credential_action(AuthState::LoggedIn, Some(old_fp), Some("new-token".into())),
            CredentialAction::Apply("new-token".into())
        );
    }

    #[test]
    fn credential_action_applies_when_fingerprint_missing_even_if_marked_logged_in() {
        // Defensive: a LoggedIn state with no recorded fingerprint (should not
        // happen post-T11, but a stale/older state) must not be treated as a
        // match — never silently skip a real token on disk.
        assert_eq!(
            decide_credential_action(AuthState::LoggedIn, None, Some("token".into())),
            CredentialAction::Apply("token".into())
        );
    }

    fn base_audio_settings() -> qbz_audio::settings::AudioSettings {
        qbz_audio::settings::AudioSettings::default()
    }

    #[test]
    fn audio_routing_changed_false_when_nothing_moved() {
        let a = base_audio_settings();
        let b = base_audio_settings();
        assert!(!audio_routing_changed(&a, &b));
    }

    #[test]
    fn audio_routing_changed_true_for_each_reinit_class_field() {
        let base = base_audio_settings();

        let mut backend = base.clone();
        backend.backend_type = Some(qbz_audio::AudioBackendType::Alsa);
        assert!(audio_routing_changed(&base, &backend), "backend_type");

        let mut device = base.clone();
        device.output_device = Some("hw:CARD=D30,DEV=0".into());
        assert!(audio_routing_changed(&base, &device), "output_device");

        let mut plugin = base.clone();
        plugin.alsa_plugin = Some(qbz_audio::AlsaPlugin::PlugHw);
        assert!(audio_routing_changed(&base, &plugin), "alsa_plugin");

        let mut hw_vol = base.clone();
        hw_vol.alsa_hardware_volume = !base.alsa_hardware_volume;
        assert!(audio_routing_changed(&base, &hw_vol), "alsa_hardware_volume");

        let mut hw_control = base.clone();
        let route = serde_json::from_str("\"alsa-route-v1|card=card:D50|pcm=hw|DEV=0\"")
            .expect("stable route key");
        hw_control.alsa_hardware_volume_controls.insert(
            route,
            qbz_audio::alsa_hardware_volume::AlsaMixerControlId {
                name: "D50 III".to_string(),
                index: 0,
            },
        );
        assert!(
            audio_routing_changed(&base, &hw_control),
            "alsa_hardware_volume_controls"
        );

        let mut excl = base.clone();
        excl.exclusive_mode = !base.exclusive_mode;
        assert!(audio_routing_changed(&base, &excl), "exclusive_mode");

        let mut pass = base.clone();
        pass.dac_passthrough = !base.dac_passthrough;
        assert!(audio_routing_changed(&base, &pass), "dac_passthrough");

        let mut lock_out = base.clone();
        lock_out.skip_sink_switch = !base.skip_sink_switch;
        assert!(audio_routing_changed(&base, &lock_out), "skip_sink_switch");

        let mut dsd = base.clone();
        dsd.dsd_mode = "dop".to_string();
        assert!(audio_routing_changed(&base, &dsd), "dsd_mode");

        let mut rate = base.clone();
        rate.device_max_sample_rate = Some(192_000);
        assert!(audio_routing_changed(&base, &rate), "device_max_sample_rate");
    }

    #[test]
    fn audio_routing_changed_false_for_reload_class_fields_only() {
        // Changing ONLY Reload-class fields must never trip a reinit.
        let base = base_audio_settings();
        let mut reload_only = base.clone();
        reload_only.gapless_enabled = !base.gapless_enabled;
        reload_only.stream_first_track = !base.stream_first_track;
        reload_only.stream_buffer_seconds = 7;
        reload_only.streaming_only = !base.streaming_only;
        reload_only.limit_quality_to_device = !base.limit_quality_to_device;
        reload_only.allow_quality_fallback = !base.allow_quality_fallback;
        reload_only.quality_fallback_behavior = "always_skip".to_string();
        reload_only.normalization_enabled = !base.normalization_enabled;
        reload_only.normalization_target_lufs = -18.0;
        reload_only.pw_force_bitperfect = !base.pw_force_bitperfect;
        reload_only.reserve_dac_while_running = !base.reserve_dac_while_running;
        reload_only.sync_audio_on_startup = !base.sync_audio_on_startup;
        assert!(!audio_routing_changed(&base, &reload_only));
    }
}
