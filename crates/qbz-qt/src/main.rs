// No console window beside the GUI on Windows (W4). The file logger
// (`qbz_log::install`) is unaffected; only the inherited stdio console goes.
// If the offscreen smoke ever comes back with an empty log because of this,
// the fix is AttachConsole(ATTACH_PARENT_PROCESS), not reverting this.
#![cfg_attr(windows, windows_subsystem = "windows")]

//! qbz-qt — Qt/QML frontend POC entry point.
//!
//! Phase 1: boot flow (splash -> silent session restore -> shell
//! placeholder | login screen) with all the offline guardrails of the
//! Slint app. `main` is NOT async: `QGuiApplication::exec()` owns the main
//! thread, so a process-global multi-thread tokio runtime carries all
//! async work; results hop back to Qt through the bridge's `CxxQtThread`.

mod auth_qt;
#[cfg(test)]
#[path = "../build_support.rs"]
mod build_support;
mod deep_link_qt;
mod listen_log_qt;
#[cfg(target_os = "linux")]
mod single_instance_qt;
// The Windows sibling. Carries its own `#![cfg(target_os = "windows")]`.
mod single_instance_win;
// Per-domain QML bridge singletons (phase 23 — the QbzBridge God-object
// split). THE PATTERN (phase-1, replicated per domain file):
//   1. one #[cxx_qt::bridge] mod per file (crate root — cxx-qt-build
//      accepts rust_files from ONE directory only, QTBUG-93443) with a
//      single #[qml_element] #[qml_singleton] QObject + its *Rust storage;
//   2. `static QT_THREAD: OnceLock<CxxQtThread<QbzDomain>>` per file;
//   3. the singleton's `boot()` invokable registers `self.qt_thread()` —
//      Main.qml boots EVERY singleton (they instantiate lazily); only
//      QbzSession.boot also fires crate::on_boot();
//   4. `pub(crate) fn ui(f)` queues a property mutation onto that
//      object's Qt event loop (no-op pre-registration).
// All singletons live in the SAME QML module (com.blitzfc.qbz); the
// invokables stay one-line forwards into the domain controllers below.
mod home_bridge;
mod player_bridge;
mod queue_bridge;
mod session_bridge;
mod panel_resize;
mod shell_bridge;
mod viz_bridge;
// Immersive mode (2026-08-02 immersive-port contract, block B1): the
// QbzImmersive singleton (§3) — open funnel + view persistence + the §3.4
// search surface. The QML overlay lands in B2.
mod immersive_bridge;
// Shader scenes (2026-08-15 immersive-completion contract, block A1): the
// QbzShaderScene singleton — scene mode + tier gate + the batched audio
// pack. `scene_bridge.rs` was TAKEN by ArtistScene (spec 01 §4 name check).
mod shader_scene_bridge;
// Immersive Suggestions (the same contract, block B4, §4.5): the
// QbzSuggestions singleton — the one genuinely absent domain.
mod suggestions_bridge;
// Hotkeys layer (2026-08-03 hotkeys-port contract, block B1): the
// QbzHotkeys singleton — the QML dispatcher's Rust brain (§1.1 pipeline).
mod hotkeys_bridge;
// App-wide "Open Music Link" modal and its frontend-neutral resolver adapter.
// The bridge is a singleton because the header, hotkey and launcher/deep-link
// paths all outlive the surface that triggered them.
mod link_resolver_bridge;
mod link_resolver_qt;
// Search domain (2026-08-03 cortinilla-parity contract, commit C0): the
// QbzSearch singleton, extracted from the QbzBridge god-object.
mod search_bridge;
// Local-library half of the cortinilla (same contract, commit C6): the pure
// + blocking mappers shared by the desktop and immersive dropdowns.
mod search_local;
// Instant cached paint for the cortinilla (contract C11, rulings R1+R6).
mod album_bridge;
mod artist_bridge;
mod cast_bridge;
mod library_bridge;
mod local_bridge;
mod musician_bridge;
mod scene_bridge;
mod search_cache_qt;
// MyQBZ splits across THREE singletons rather than one, because the two
// modals are global overlays with their own lifetime: QbzMyQbz carries the
// two grids + the detail page + the edit/mix modals + the branding,
// QbzMyQbzAdd carries only the app-wide "Add to Mixtape/Collection" picker
// (mounted in AppShell, reachable from any view's row menu), and QbzDisco
// carries the Artist-Collection builder.
mod blacklist_bridge;
mod disco_bridge;
mod myqbz_add_bridge;
mod myqbz_bridge;
mod playlist_picker_bridge;
// Playlist Manager. THREE singletons, not one (contract D2): the manager
// document + folder list + organisation writes here, the folder modals on
// QbzFolderEdit and the shared playlist editor on QbzPlaylistEdit — so a
// folder save cannot perturb an open playlist editor.
mod folder_edit_bridge;
mod font_qt;
mod playlist_edit_bridge;
mod playlist_manager_bridge;
// Purchases (2026-08-15 purchases contract §G.1): the QbzPurchases singleton —
// two documents (list + album detail) and a publish counter. Its controller
// half is `purchases_qt` below, a PLAIN module. This file DOES belong in
// build.rs's rust_files (lane D owns that edit); without it the QML singleton
// silently does not exist.
mod purchases_bridge;
// Playlist Importer (public Spotify / Apple Music / Tidal / Deezer playlists).
// Its own singleton: a separate domain from both the playlist detail and the
// manager, opened from two shell surfaces that outlive each other, and its
// modal must survive the one that opened it (05 §5.8).
mod playlist_import_bridge;
// "Find available version" (2026-08-17 unavailable-tracks contract §6): the
// QbzTrackReplace singleton — the replacement modal for a track Qobuz pulled
// from the catalogue. Its own singleton for the same reason as its modal
// neighbours: it is mounted once in AppShell, and its apply reloads the
// playlist underneath it. Its controller half is `track_replace_qt` below, a
// PLAIN module. This file DOES belong in build.rs's rust_files; without it the
// QML singleton silently does not exist.
mod track_replace_bridge;
// HiFi Wizard (DAC setup). Its own singleton rather than more surface on
// QbzBridge: the wizard is one self-contained modal whose document nobody else
// reads, and its read-back ticks every 1.5 s while the test plays — routing
// that through the settings document would republish the whole Settings view
// on every tick. Its controller half is `dac_wizard_qt` (a PLAIN module).
mod dac_wizard_bridge;
// Qobuz Connect (2026-08-01 contract §8, block B4-Rust): the QbzQConnect
// singleton — the QML surface of the facade/sink controllers below
// (qconnect_qt.rs / qconnect_event_sink_qt.rs).
mod qconnect_bridge;
// Kiosk zone navigation (2026-08-02 kiosk-port contract §7): the
// QbzKioskNav singleton over the state machine in kiosk_nav_qt.rs. It has
// no boot() — nothing in Rust ever publishes into it (see its header).
mod kiosk_nav_bridge;
// Miniplayer (2026-08-03 miniplayer/tray contract, block B1): the QbzMini
// singleton — the port of the Slint MiniPlayerState global. Its controller
// half is `mini_qt` below, which is a PLAIN module.
mod mini_bridge;
// System tray (the same contract, block B5): the QbzTray singleton — the
// window verbs' Qt-thread hop. Its controllers are `tray_qt` (portable) and
// `tray_linux` (the ksni item), both PLAIN modules below.
mod kiosk_nav_qt;
mod tray_bridge;
// The kiosk profile itself (the same contract, §8): env/pref resolution, the
// live Kiosk <-> Desktop toggle, and the boot decisions that follow from it.
mod ab_loop_qt;
mod artwork_qt;
mod atmosphere_qt;
mod kiosk_profile_qt;
// THE audible step (design 02 §9 stage 3): one exhaustive match over
// `qbz_source::PlaybackTicket`, replacing the three drifted copies of
// `play_audible` that each matched on `track.source` by hand.
mod audible_qt;
mod bridge;
// The registry's injections: Plex credentials, the ephemeral store, the user
// binding and the album-identity mode. See its header.
mod source_wiring;
// Jellyfin / Subsonic: the per-user settings store plus the two gates the
// Local Library union reads. The credential glue itself is in `source_wiring`.
mod media_servers_qt;
mod media_connection_qt;
// Profile-scoped metadata overlays for Plex/Jellyfin/Subsonic. Physical files
// keep using `.qbz.json`; this module is the read-through cache for servers
// that have no local directory to put that document beside.
mod remote_metadata_qt;
// The library sweep. Lives in the frontend because it needs the tokio runtime
// and a channel to the progress UI; it writes through the cache handle the
// SOURCE owns, so sweep and reads can never point at different users.
mod custom_theme_qt;
mod diagnostics_qt;
mod discover_config_qt;
mod fav_cache_qt;
mod media_sync_qt;
// The folder-modal controller. A plain module — it declares no
// #[cxx_qt::bridge], so it must NOT appear in build.rs's rust_files.
mod folder_edit_qt;
mod folders_qt;
// The HiFi Wizard controller. A plain module — it declares no
// #[cxx_qt::bridge], so it must NOT appear in build.rs's rust_files. All of
// its COMPUTATION lives in the shared `qbz-dac-wizard-core` crate, which the
// Slint adapter uses too: the wizard emits PipeWire/WirePlumber snippets the
// user pastes into their own system, so two implementations would be the one
// divergence this port must never produce.
mod dac_wizard_qt;
// The renderer TIER — one source of truth for what QRhi actually gave us and
// what may be offered because of it (the Qt analogue of Slint's
// `use_gpu_renderer`). A plain module: it declares no #[cxx_qt::bridge], so it
// must NOT appear in build.rs's rust_files.
mod foryou_qt;
mod genre_filter_qt;
mod home_qt;
mod icon_tint_qt;
mod recently_qt;
mod renderer_qt;
// The in-app log viewer over `qbz_log::ring` — the read surface that turns
// "Share logs" from an `open::that(path)` handoff into a filterable view with
// copy / bundle / upload.
mod log_viewer_qt;
// About QBZ + What's New (the header menu's last two rows). `about_bridge`
// declares the #[cxx_qt::bridge] and IS listed in build.rs's rust_files; the
// other two are plain controller modules and must NOT be.
mod about_bridge;
mod about_qt;
mod updates_qt;
mod library_bulk;
mod library_db_qt;
mod library_prefs;
mod library_qt;
mod library_sidepanel;
mod local_artist_images_qt;
mod local_artist_match;
mod local_catalog_qt;
mod local_filter;
mod local_library_qt;
mod local_rows;
mod local_state;
mod local_service_qt;
mod orbit_bridge;
mod orbit_qt;
mod recommendations_qt;
mod whats_new_qt;
// Watcher hints + periodic root reconciliation for the incremental scanner.
mod local_scan_maintenance_qt;
// Native bounded Tracks QAbstractListModel + catalog query controller (phase E).
// Plain Rust module; the QML type itself is hand-written C++ under cxx/.
mod local_tracks_model_qt;
// Native bounded Albums entry model (phase F1); grid/list geometry stays QML.
mod local_albums_model_qt;
// Native paged Artists rail + indexed artist-album pane (phase F2).
mod local_artists_model_qt;
mod local_plex;
// Plex PIN sign-in (the "Authorize" half of the Plex settings) + the
// Check-connection ping. Over `qbz_plex`'s existing pin/start, pin/check and
// ping calls — the protocol was always in the shared crate, only the glue was
// missing.
mod local_album_actions;
mod local_albums;
mod local_artwork;
mod local_bridge_ops;
mod local_bulk;
mod local_ephemeral;
mod local_playback;
mod local_playlist_qt;
mod local_restore_qt;
mod local_tree;
mod plex_pin_qt;
// MyQBZ domain controllers. One module per concern, all driven by the three
// bridges above: the grids + Create modal (myqbz_qt), the per-user branding
// and per-collection view prefs (myqbz_prefs_qt), the detail page
// (myqbz_detail_qt) and its playback / edit / cover / DJ-mix arms, the
// app-wide Add picker (myqbz_add_qt) and the Artist-Collection builder
// (myqbz_builder_qt + its Qobuz/local/Plex fetchers).
mod myqbz_add_qt;
mod myqbz_builder_fetch_qt;
mod myqbz_builder_qt;
mod myqbz_cover_qt;
mod myqbz_detail_qt;
mod myqbz_edit_qt;
mod myqbz_mix_qt;
mod myqbz_play_qt;
mod myqbz_prefs_qt;
mod myqbz_qt;
// Blacklist: the per-user store (artist_blacklist), the Recommendations
// dismissal store (reco_dismiss_qt) and the manager view's controller.
mod artist_blacklist;
mod blacklist_qt;
mod reco_dismiss_qt;
// Artist page per-section release sort, persisted by release_type. Co-owns
// `<data-dir>/qbz/artist_ui.json` with the Slint's crates/qbz/src/artist_prefs.rs
// exactly the way library_prefs co-owns favorites_ui.json.
mod artist_prefs;
// The dedicated discography page — one release bucket of one artist, paged.
// Reached from the artist page's "See discography" and from the album page's
// "From the same artist" View all. A plain module, NOT a bridge: it publishes
// onto QbzArtist.artistReleasesJson (artist_bridge.rs) and records its own nav
// entry, the label_qt shape.
mod artist_releases_qt;
mod artist_scene_qt;
mod musician_qt;
// Shared in-app toast publisher (the port of qbz-slint-common's toast.rs).
// A plain module, NOT a bridge — it publishes onto QbzShell.toastJson, and
// `controls/QbzToast.qml` owns the auto-hide timer.
mod toast_qt;
// Share links + the system clipboard (the port of crates/qbz/src/share.rs).
// A plain module, NOT a bridge — the artist header's ⋯ → Share reaches it
// through QbzArtist.share (artist_bridge.rs).
mod browse_qt;
mod cast_qt;
mod cast_viz;
mod cdda_qt;
mod disc_identity;
mod disc_meta_bridge;
mod disc_meta_qt;
mod label_qt;
mod local_media_info_qt;
mod nav_qt;
mod page_restore_qt;
mod wallpaper_qt;
#[cfg(target_os = "linux")]
mod wallpaper_wayland_qt;
mod now_playing;
mod offline_fwd;
mod output_labels;
mod quality_qt;
mod quality_state;
mod rip_qt;
mod rip_wizard_qt;
mod sacd_qt;
mod share_qt;
mod store_qt;
mod tag_editor_bridge;
mod tag_editor_qt;
// Offline cache (downloads tier): state activation on login + the action
// set. offline_fwd.rs is the offline MODE (connectivity); these two are the
// user-managed download cache (see AGENTS.md's caching-model note).
mod offline_cache_qt;
mod offline_manager_bridge;
mod offline_manager_qt;
mod offline_qt;
// Shared multi-select bulk actions for Qobuz track listings (playlist,
// artist, label — the album page has its own album-ordered variant).
mod album_qt;
// Lightweight, navigation-free AlbumCard preview. It owns a generation-
// guarded document separate from the full AlbumView domain.
mod album_quick_view_qt;
mod award_qt;
mod bulk_tracks_qt;
mod track_info_qt;
// Album Info (Credits/Review) modal controller — info_modals.rs port.
mod album_info_qt;
// Album custom covers + cover file actions — the album half of the Slint
// `custom_artwork.rs` store (SAME json file, shared between both apps).
mod ambient_qt;
mod cover_artwork_qt;
mod external_reco_qt;
// Tunnel Flow scene (B1, 2026-08-15 immersive-completion contract): the
// Tauri line-palette extraction, published on QbzShaderScene per track.
mod artist_qt;
mod lyrics_qt;
mod playback_qt;
mod playlist_index_qt;
mod playlist_picker_qt;
mod tunnelflow_qt;
// Playlist Importer controller (the `qbz-playlist-import` crate's frontend
// half). Plain module — it declares no #[cxx_qt::bridge], so it must NOT
// appear in build.rs's rust_files.
mod playlist_import_qt;
// Playlist Manager controller, split three ways up front (the reference is
// 1053 lines BEFORE the two state machines and two serializers this port
// adds): _qt = load / cache / toolbar / publish, _rows = the pure model
// functions, _ops = the optimistic mutations. Plain modules — they declare no
// #[cxx_qt::bridge], so they must NOT appear in build.rs's rust_files.
mod playlist_manager_ops;
mod playlist_manager_qt;
mod playlist_manager_rows;
mod playlist_snapshot_qt;
// The SHARED playlist editor's controller (rename · description ·
// offline-only · delete), driven from the manager's three delegates, the
// sidebar row menu and the playlist detail header. Plain module — it declares
// no #[cxx_qt::bridge], so it must NOT appear in build.rs's rust_files.
mod playlist_create_qt;
mod playlist_edit_qt;
mod playlist_qt;
// The replacement-search controller (2026-08-17 unavailable-tracks contract
// §6): ISRC short-circuit, the ranked text search, and the add -> reposition ->
// remove apply order. Plain module — it declares no #[cxx_qt::bridge], so it
// must NOT appear in build.rs's rust_files.
mod track_replace_qt;
// Purchases controller (2026-08-15 purchases contract): the two screens' state,
// the FRONTEND album-download loop (concurrency 1, cancellable, per-track
// progress) and the format re-scoping. Plain module — it declares no
// #[cxx_qt::bridge], so it must NOT appear in build.rs's rust_files.
mod purchase_playback_qt;
mod purchases_qt;
mod queue_qt;
mod search_qt;
// Immersive Suggestions controller (2026-08-02 immersive-port contract §4.5,
// block B4): the loader + action arms behind QbzSuggestions. Plain module —
// no #[cxx_qt::bridge], so it must NOT appear in build.rs's rust_files.
mod suggestions_qt;
// Hotkeys layer PURE core (2026-08-03 hotkeys-port contract §3, block B1):
// the 23-action table, the shortcut grammar, the ui_prefs.json `keybindings`
// store, capture, the groups builder and the §1.2 Escape stack — the byte-
// faithful port of the Slint crates/qbz/src/keybindings.rs. Plain module —
// no #[cxx_qt::bridge], so it must NOT appear in build.rs's rust_files.
mod hotkeys_qt;
mod integrations_qt;
mod link_handler_qt;
mod settings_qt;
mod sidebar_qt;
mod sleep_timer_qt;
mod theme_qt;
mod viz_qt;
// Qobuz Connect port (2026-08-01 contract), blocks B1/B2: transport config +
// credential discovery + device identity, and the renderer engine over
// `runtime.core()`. Plain modules — they declare no #[cxx_qt::bridge], so
// they must NOT appear in build.rs's rust_files. Wired by the B3 facade.
mod qconnect_delegation_qt;
mod qconnect_engine_qt;
mod qconnect_lan_qt;
mod qconnect_transport_qt;
// QConnect port block B3: the facade (service singleton + controller routing)
// and the inbound event sink. Plain modules, same convention as B1/B2. The
// §11.5 wiring (init_service + spawns) lives in `on_session_entered` below.
mod qconnect_event_sink_qt;
mod qconnect_qt;
// Miniplayer controller (the same contract, block B1, §4.4.1/§4.7): the queue
// projection, the prefs seeds and the row/geometry arithmetic behind QbzMini.
// Plain module — it declares no #[cxx_qt::bridge], so it must NOT appear in
// build.rs's rust_files.
mod mini_qt;
// Tray controllers (the same contract, block B5, §5). `tray_qt` is portable
// and holds the gates, the debounce and the transport verbs; `tray_linux` is
// the ksni StatusNotifierItem and carries no inner cfg, so the gate is on the
// mod line (the shape of `crates/qbz/src/tray/mod.rs:26-27`). Owner ruling K2
// keeps macOS and Windows out — neither has a tray in either frontend.
#[cfg(target_os = "linux")]
mod tray_linux;
mod tray_qt;
// macOS menu-bar tray (NSStatusItem), ported 2026-08-05. The K2 ruling that
// kept macOS out was reasoned from a premise that reads the reference
// backwards — see the module header.
#[cfg(target_os = "macos")]
mod tray_macos;
// macOS custom chrome: the overlay window attributes + centring the native
// traffic lights in the 42px header. Portable module (a no-op stub off
// macOS) rather than a gated `mod` line, so the QbzShell invokable that
// calls it needs no cfg of its own.
mod macos_chrome;
// Ungated: the C++ behind it is portable Qt, so every platform
// type-checks the seam even though only Windows uses the handle.
mod win_shell;

/// Flush the session while Windows waits on WM_QUERYENDSESSION.
///
/// BOUNDED, and that is the whole design. Qt blocks inside `commitDataRequest`
/// until this returns, and Windows starts terminating applications that have
/// not answered after roughly five seconds -- so an unbounded save here does
/// not protect the session, it gets the process killed halfway through writing
/// it, which is worse than not trying.
///
/// So the save runs on its own thread and the GUI thread waits on a deadline
/// comfortably under the OS limit. A `tokio::time::timeout` around the future
/// would not do: it cannot preempt the synchronous SQLite section, which is
/// the part that can actually take time.
///
/// If the deadline passes we return anyway and let the session end. The write
/// either lands in the remaining moments or it does not; either way the shell
/// is not left waiting, and the log says which happened.
///
/// `catch_unwind` because a panic must never cross the C ABI back into Qt's
/// signal emission.
#[cfg(target_os = "windows")]
extern "C" fn on_session_commit_data() {
    use std::sync::mpsc::channel;
    use std::time::Duration;

    /// Well under the ~5 s Windows allows before it stops waiting.
    const COMMIT_DEADLINE: Duration = Duration::from_secs(3);
    /// Leave one second of the Windows budget for the bounded SQLite flush.
    const QCONNECT_DEADLINE: Duration = Duration::from_secs(2);

    let _ = std::panic::catch_unwind(|| {
        log::info!("[qbz-qt] session ending; withdrawing QConnect before persistence");
        let (tx, rx) = channel::<()>();
        let spawned = std::thread::Builder::new()
            .name("qbz-session-commit".to_string())
            .spawn(move || {
                // A delegated queue is session-scoped and must never become
                // the owner's Windows shutdown snapshot. Use the same ordered
                // teardown as the ordinary event-loop exit, but reserve time
                // for the synchronous SQLite flush below. Fail closed: when
                // authority cannot be restored within the budget, preserve
                // the previous on-disk snapshot instead of writing guest data.
                let qconnect_owner_safe = match (TOKIO.get(), qconnect_qt::service()) {
                    (_, None) => true,
                    (Some(runtime), Some(service)) => runtime.block_on(async move {
                        match tokio::time::timeout(QCONNECT_DEADLINE, service.disconnect()).await {
                            Ok(Ok(())) => true,
                            Ok(Err(error)) => {
                                log::warn!(
                                    "[qbz-qt] Windows QConnect teardown failed: {error}"
                                );
                                false
                            }
                            Err(_) => {
                                log::warn!(
                                    "[qbz-qt] Windows QConnect teardown exceeded {}s",
                                    QCONNECT_DEADLINE.as_secs()
                                );
                                false
                            }
                        }
                    }),
                    (None, Some(_)) => {
                        log::warn!(
                            "[qbz-qt] Windows QConnect teardown has no runtime; skipping persistence"
                        );
                        false
                    }
                };
                // `save_on_exit` block_on()s the capture on the tokio handle
                // bound at shell entry. Safe from a plain thread; it would
                // panic only from inside an async context, which this is not.
                if qconnect_owner_safe {
                    qbz_app::session_persist::save_on_exit();
                } else {
                    log::warn!(
                        "[qbz-qt] Windows session persistence skipped to avoid saving delegated state"
                    );
                }
                // Listen log: the row in flight closes as `shutdown` here too.
                listen_log_qt::shutdown_blocking();
                let _ = tx.send(());
            });
        if let Err(e) = spawned {
            log::warn!("[qbz-qt] could not spawn the session-commit thread: {e}");
            return;
        }
        // Timeout and Disconnected are DIFFERENT and must not share a line: a
        // panic in the worker drops the sender, which arrives here as
        // Disconnected, and reporting that as "did not finish in time" would
        // hide a crash behind a slow-disk story. The outer catch_unwind cannot
        // see it -- it guards this thread, not that one.
        match rx.recv_timeout(COMMIT_DEADLINE) {
            Ok(()) => log::info!("[qbz-qt] session persisted before the session ended"),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => log::warn!(
                "[qbz-qt] session persist did not finish within {}s; letting the session end",
                COMMIT_DEADLINE.as_secs()
            ),
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                log::error!("[qbz-qt] the session-persist thread died before reporting")
            }
        }
    });
}
// Windows tray. The file carries its own `#![cfg(target_os = "windows")]`,
// matching how tray_macos/tray_linux are declared.
mod tray_windows;
// MPRIS / media keys (owner ruling K3, REVERSED by the owner on 2026-08-04
// after smoking the tray: "no aparece por ejemplo en el widget de now playing
// de KDE Plasma"). Plasma reads MPRIS, so this is what makes the desktop see
// QBZ at all. Portable module over the shared `qbz-media-controls` crate,
// exactly like the Slint and qbzd consumers; the crate does its own cfg
// gating, so this mod line is not platform-gated.
mod media_controls_qt;

use std::collections::HashSet;
use std::pin::Pin;
use std::sync::{Arc, LazyLock, Mutex, OnceLock};

use cxx_qt::CxxQtThread;
use cxx_qt_lib::{QGuiApplication, QQmlApplicationEngine, QString, QUrl};
use qbz_app::shell::AppRuntime;
use qbz_core::LoggingAdapter;
use tokio::runtime::Runtime;
use tokio::task::JoinHandle;

use bridge::qbz_bridge::QbzBridge;

/// Same URL the Slint shell opens for the Terms-of-Service link.
pub(crate) const QOBUZ_TOS_URL: &str = "https://www.qobuz.com/us-en/legal/terms";

/// The async runtime for everything non-Qt (OAuth, session restore,
/// connectivity monitoring). Lives for the whole process.
static TOKIO: OnceLock<Runtime> = OnceLock::new();

/// The UI-agnostic composition root (core + runtime state + session).
static APP: OnceLock<Arc<AppRuntime<LoggingAdapter>>> = OnceLock::new();

/// Qt-thread handle of the singleton bridge, registered by its first
/// invokable (`boot`). `CxxQtThread` is Send+Sync and safe to queue on from
/// any thread; before registration, `ui()` is a no-op.
static QT_THREAD: OnceLock<CxxQtThread<QbzBridge>> = OnceLock::new();

/// In-flight browser-OAuth task, so `cancelLogin()` can `abort()` it
/// (mirrors `login_task` in crates/qbz/src/main.rs).
static LOGIN_TASK: Mutex<Option<JoinHandle<()>>> = Mutex::new(None);

/// Whether the Home view has been loaded this session (the auto-load fires
/// once per shell entry; `reloadHome()` bypasses the flag).
static HOME_LOADED: Mutex<bool> = Mutex::new(false);

/// Whether the favourite-id cache has had its network refresh this session
/// (`warm_favorites_once`). Reset on logout with the rest.
static FAV_WARMED: Mutex<bool> = Mutex::new(false);

/// Whether the sidebar tree has been built this session (`load_sidebar_once`).
///
/// MODULE-LEVEL on purpose. It used to be a function-local `static` inside
/// `load_sidebar_once`, which made it PROCESS-scoped with no way to reach it
/// from `do_logout` — so after logout -> login the latch was still set, the
/// tree was never rebuilt for the new session, and the sidebar stayed on the
/// `"[]"` the logout block publishes until the user found the Refresh row.
/// Owner-reproduced 2026-07-31 (qbz.log 20:33:32 login -> no `sidebar loaded`
/// until 20:33:47, which is the manual refresh). Reset with its siblings below.
static SIDEBAR_LOADED: Mutex<bool> = Mutex::new(false);

/// Drop every "once per session" latch. Called at each session ENTRY
/// (`on_session_entered`) and again on logout, so neither a logout -> login nor
/// an offline -> login transition can leave a new session reading the previous
/// one's answer. Each latch's own doc explains what it gates.
/// Restore the persisted queue + current track, once per process.
///
/// PAUSED by design (the shared module's "Phase A"): the queue and the cursor
/// come back, the audio does not start itself. The saved position rides along
/// and is consumed by the first play of that same track
/// (`session_persist::take_resume_for`, threaded through
/// `play_resolved_offline_aware`).
fn restore_session_once() {
    static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if DONE.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    // Bind the exit context here rather than at startup: this is the first
    // moment a runtime exists AND a user session owns a store, so a snapshot
    // taken on quit can never belong to nobody. `OnceLock` — later entries are
    // ignored, which is what we want (the runtime is process-global anyway).
    qbz_app::session_persist::bind_exit_ctx(
        app(),
        TOKIO
            .get()
            .expect("tokio runtime set before the shell mounts")
            .handle()
            .clone(),
    );
    // Crash-chain rung 3: two consecutive boots died even after the view reset,
    // so skip the queue restore for THIS boot only. The persisted queue stays
    // on disk untouched — a healthy boot brings it back.
    if nav_qt::crash_level() >= 3 {
        log::warn!("[crash-chain] session restore bypassed this boot (queue kept on disk)");
        return;
    }
    spawn(async move {
        let runtime = app();
        if !qbz_app::session_persist::restore(&runtime).await {
            return;
        }
        // The queue exists in the core now; the bar and the queue panel repaint
        // from explicit refreshes, not from the core mutation.
        playback_qt::refresh_now_playing(&runtime).await;
        // AFTER the meta publish, never before: `refresh_now_playing` goes
        // through `set_track`, which arms loading+playing for the real play
        // path. Nothing was dispatched here, so those have to be taken back
        // down or the app opens with the play button spinning.
        now_playing::mark_restored_idle();
        playback_qt::publish_queue(&runtime).await;
        let resume = qbz_app::session_persist::pending_resume_position();
        // #683: `refresh_now_playing` publishes the OPTIMISTIC MPRIS `Playing`
        // at 0 (a stream is normally about to open, and the poll loop's
        // play/pause edge corrects it). Nothing opens here — the restored
        // session is parked — so without this `playerctl status` answered
        // "Playing" from boot until the first real play, and the sleep
        // inhibitor was armed for a paused app. Paused, at the resume
        // position, is the truth.
        media_controls_qt::push_playback_state(false, resume);
        log::info!("[qbz-qt] session restored (resume position {resume}s)");
    });
}

fn reset_session_latches() {
    *HOME_LOADED.lock().unwrap() = false;
    *LIBRARY_LOADED.lock().unwrap() = false;
    *FAV_WARMED.lock().unwrap() = false;
    *SIDEBAR_LOADED.lock().unwrap() = false;
}

pub(crate) fn register_qt_thread(thread: CxxQtThread<QbzBridge>) {
    if QT_THREAD.set(thread).is_err() {
        log::warn!("[qbz-qt] Qt thread handle already registered");
    }
}

pub(crate) fn app() -> Arc<AppRuntime<LoggingAdapter>> {
    Arc::clone(APP.get().expect("AppRuntime not initialized"))
}

/// Spawn a future on the process-global tokio runtime.
pub(crate) fn spawn(future: impl std::future::Future<Output = ()> + Send + 'static) {
    TOKIO
        .get()
        .expect("tokio runtime not initialized")
        .spawn(future);
}

/// Queue a bridge mutation onto the Qt event loop — the cxx-qt analogue of
/// Slint's `upgrade_in_event_loop`. No-op before the bridge registers its
/// thread handle (boot's first step) or after it is destroyed.
pub(crate) fn ui(f: impl FnOnce(Pin<&mut QbzBridge>) + Send + 'static) {
    if let Some(thread) = QT_THREAD.get() {
        let _ = thread.queue(f);
    }
}

/// Boot sequence, fired by the bridge's `boot` invokable (Main.qml
/// Component.onCompleted — the Qt-thread handle is registered first, so
/// every `ui()` hop below lands).
pub(crate) fn on_boot() {
    // The source registry, FIRST of all — before anything can call
    // `qbz_source::registry()`. It is a OnceLock, so whoever touches it first
    // BUILDS it, and a registry built without the client lens leaves the Qobuz
    // source permanently detached: every catalog call then answers
    // `NotConfigured`, loudly but uselessly. Measured exactly that way on the
    // first wiring pass — the local and Plex rows resolved while all 26 Qobuz
    // albums came back "no client lens installed".
    //
    // A LENS, not a cached clone: `qbz-core` REPLACES its client (core.rs:346
    // and :384, both from paths that run before `set_session`), so anything
    // holding a clone goes stale and fails silently. Reading through on every
    // call is what makes that unrepresentable. The read guard lives inside the
    // returned future and is dropped with it, which is the same discipline
    // `myqbz_play_qt` documents for its own clone-then-drop.
    qbz_source::init_registry(std::sync::Arc::new(|| {
        Box::pin(async {
            let lock = app().core().client();
            let guard = lock.read().await;
            guard.as_ref().cloned()
        })
    }));
    // ...and its INJECTIONS immediately after. The registry is built lazily by
    // whoever touches it first, so publishing these here — before any view can
    // resolve a row — is what keeps a Plex cover or a local play from asking an
    // unwired source (see `source_wiring`'s header for what that looked like).
    source_wiring::install();

    // Offline guardrails FIRST (before the login screen can show): engine +
    // connectivity actor, then the live status forwarder. Both need the
    // tokio runtime context for their spawns.
    spawn(async {
        offline_fwd::start();
    });
    offline_fwd::start_ui_forwarder();

    // Image-cache housekeeping, off the Qt thread, once per run: the shared
    // `~/.cache/qbz/images` LRU trim (200 MB, batched under the cache mutex)
    // and the `images/scaled` orphan sweep + byte cap. See artwork_qt.rs
    // "Cache eviction".
    spawn(async {
        let _ = tokio::task::spawn_blocking(artwork_qt::housekeeping).await;
    });

    // The derived Local Library catalog is deliberately delayed past boot's
    // useful frame and runs entirely on the blocking pool. It reads the three
    // authoritative caches read-only, yields between 250-row transactions and
    // leaves every current view on its existing reader until E opts Tracks in.
    local_catalog_qt::start();
    local_scan_maintenance_qt::start();

    // Splash -> silent session restore -> shell | login.
    let runtime = app();
    spawn(async move {
        // Best-effort, offline-tolerant (mirrors the Slint boot): a network
        // failure here leaves the core usable for offline playback.
        if let Err(e) = runtime.init().await {
            log::warn!("[qbz-qt] core init failed (continuing): {e}");
        }
        auth_qt::finish_boot_restore(
            auth_qt::restore_saved_session(&runtime).await,
            enter_shell,
            |error| {
                session_bridge::ui(move |mut b| {
                    b.as_mut()
                        .set_restore_error(QString::from(error.as_deref().unwrap_or("")));
                    b.as_mut().set_screen(QString::from("login"));
                })
            },
        );
    });
}

/// Everything a SESSION ENTRY owes the shell, whatever the door was.
///
/// Extracted from [`enter_shell`] because `start_offline` — the "Start offline"
/// button, the ONLY way into the app for a user with no network — did NOT run
/// any of it. It recorded the nav entry, pushed the now-playing model and
/// switched the screen; the sidebar tree, the 1 Hz playback pump, the
/// integrations runtime, the streaming-quality seed, the settings document and
/// the persisted volume were all skipped. On a warm session (login first, then
/// offline) most of that had already run in the same process and hid the hole;
/// on a COLD offline start the shell came up with an empty sidebar (no folders,
/// no LOCAL playlists — the entire library for an account-less user), no
/// playback pump and the volume at 100 % on an exclusive-mode DAC.
///
/// Every step below is either idempotent or self-gates offline
/// (`load_home_once` / `warm_favorites_once` return early and, importantly,
/// leave their latch UNSET so a later online session still runs them), so the
/// two callers can share it verbatim.
fn on_session_entered() {
    // The once-per-session latches, reset HERE and not only in `do_logout`:
    // logging in FROM an offline session (the D2 recovery banner, or the login
    // screen after "Start offline") never passes through logout — verified in
    // the owner's 2026-07-31 log, where the OAuth exchange at 20:33:32 follows
    // the offline entry at 20:32:38 with no `logged out` line between them. A
    // latch reset only at logout would leave that transition with the OFFLINE
    // sidebar (folders + locals, no Qobuz playlists) for the rest of the run.
    // Every caller of this function is one genuine session entry, so this is
    // the one place that sees them all.
    reset_session_latches();
    // Phase 1b: seed the local device-cap cache (#638 fix 3) as early in the
    // session as possible, so the first governed play resolves against the cap
    // and the Settings "Detected device limit" row is filled on first open.
    // Instant no-op while the toggle is off, which is the default.
    //
    // Spawned, not awaited — the probe shells out to `pw-dump` and this
    // function runs on the Qt thread. The documented consequence (same trade
    // the reference accepts): a session-restore play that beats the refresh
    // gets ONE uncapped track. It self-heals on the next play.
    //
    // The republish is NOT optional. The boot snapshot is spawned separately
    // and can serialize BEFORE this probe lands, so without a publish of our
    // own the Settings row stays absent for a user who already had the toggle
    // on — until some unrelated mutation happens to republish. Nothing else
    // re-runs on probe completion.
    spawn(async {
        settings_qt::refresh_device_cap(&app()).await;
        settings_qt::publish_snapshot().await;
    });
    // Phase 2: the shell mounts on the startup view; seed the nav history and
    // push the current now-playing model onto the bar.
    //
    // The entry view gets the SAME lazy load a sidebar click would have given
    // it (`hydrate_view`). Without it "Where you left off" restored onto
    // Library / Mixtapes / Collections and painted an EMPTY page — the view is
    // fed by a Rust document that only `navigate_to` ever asked for, and a
    // restore does not go through `navigate_to`.
    // Always consume the ordinary one-shot entry decision, even when this is
    // an unauthenticated offline session. A later OAuth login in the SAME
    // process must keep the existing fresh-session behaviour (Home), not
    // replay a remembered startup route that the offline entry skipped.
    let ordinary_entry = nav_qt::shell_entry_view();
    let logged_off = offline_fwd::engine().status().offline_session;
    let local_album = local_restore_qt::take_startup_album();
    // The exact page (page_restore_qt: a detail, a root's tab, a Settings
    // section, a search). Consumed here even when another door wins, so a
    // re-entry in the same process never replays it. `ordinary_entry` already
    // names its entry view — the shell mounted that view at construction.
    let page = page_restore_qt::take_startup_page();
    let entry_view = if local_album.is_some() {
        "localalbum".to_string()
    } else if logged_off {
        "local".to_string()
    } else {
        ordinary_entry
    };
    nav_qt::record(&entry_view);
    hydrate_view(&entry_view);
    // The desktop wallpaper, when a background mode paints it (a no-op otherwise).
    wallpaper_qt::refresh();
    // Logged-off startup is the ONLY automatic redirect. An authenticated
    // account keeps the established startup/restore flow even when physical
    // connectivity is down. The selected tab is the first user-ordered Local
    // Library tab; kiosk takes the first surface it can render (Genres is a
    // desktop column browser).
    if let Some(route) = local_album {
        local_bridge::restore_album(route);
    } else if logged_off {
        let landing = settings_qt::local_landing_tab(kiosk_profile_qt::active());
        navigate_to_tab("local", &landing);
    } else if let Some(page) = page {
        // Re-run the page's opener on the view the shell already mounted. A
        // page that cannot come back (a catalog page while offline, a hidden
        // Purchases surface, stale arguments) steps back onto the root that
        // was seeded beneath it.
        if !page_restore_qt::restore(&page) {
            nav_qt::back();
            hydrate_view(&nav_qt::current_view());
        }
    }
    now_playing::publish_current();
    // Phase 3: fetch Discover > Home (online sessions only — the offline
    // engine gates Qobuz calls anyway, and the view shows the offline
    // placeholder instead).
    load_home_once();
    // Phase 4: the 1 Hz playback state pump (idempotent).
    playback_qt::start_poll_loop(app());
    // Phase 4b: session restore (queue + current track, PAUSED). One-shot per
    // process — `on_session_entered` is multi-entry (login, session restore,
    // "Start offline") and restoring again on a re-login would clobber whatever
    // the user has queued since. Runs AFTER the poll loop so the track-change
    // edge is already listening when the restored cursor lands.
    restore_session_once();
    // Integrations runtime: applies the persisted MusicBrainz / Discord /
    // ListenBrainz opt-ins and starts the scrobble-queue flush watcher.
    // Strictly opt-in — every one of them is inert until the user connects it.
    integrations_qt::start(&app());
    // Drop a `qobuzapp://` claim the user never made (2.0.x MSI); never
    // claims on its own — see link_handler_qt.
    link_handler_qt::reconcile_at_startup();
    // System tray (Linux only, ksni — owner ruling K2), the port of
    // `init_shell_for_user`'s `tray::init` call (`crates/qbz/src/main.rs:257-263`).
    // Suppressed under gamescope by the same predicate the reference uses
    // (`:262`): an extra mapped surface can steal the compositor's
    // focused-window pick on the Deck. This function is MULTI-ENTRY (login,
    // session restore, "Start offline"), and `init`'s three gates are what make
    // that safe — the one-shot is checked AFTER the enabled gate, so a disabled
    // first call does not burn it and a later recovery login can still arm the
    // tray. THIS is also the first `settings_qt::tray()` call in the process,
    // by design: it runs after the per-user store binds, where the bridge's
    // construction-time seed would have latched an empty one.
    {
        let tray = settings_qt::tray().get_settings().unwrap_or_default();
        tray_qt::init(
            tray.tray_icon_theme.clone(),
            tray.enable_tray && !tray_qt::in_gamescope(),
        );
    }
    // MPRIS, beside the tray exactly as the reference puts media_controls
    // beside tray::init (`crates/qbz/src/main.rs:257` then `:268`). No enable
    // flag and no gamescope predicate — the reference has neither, and a
    // desktop that cannot see the player is the complaint this closes.
    // Idempotent, so the multi-entry nature of this function is safe.
    media_controls_qt::init();
    // Phase 7: the sidebar playlist tree.
    load_sidebar_once();
    // Refresh the favourite-id cache from the network. The disk seed already
    // ran at session activation (auth_qt -> fav_cache_qt::init_for_user);
    // this is the reference's shell-entry warm (crates/qbz/src/main.rs:418-500)
    // and it is what keeps album / artist / track / label hearts — and the
    // toggle direction behind them — correct without ever opening Library.
    warm_favorites_once();
    // Phase 10: seed the playback request tier from the persisted
    // streaming-quality pref (Settings > Audio writes it live after this).
    playback_qt::set_streaming_quality(&settings_qt::streaming_quality());
    // Seed the settings document ONCE at shell entry. It used to be published
    // only by `navigate_to("settings")` and by a language change, so until the
    // user opened Settings the shell read an EMPTY doc — and the now-playing
    // bars' "Audio settings" flyout reads `normalization` / `gapless` from it.
    // That is what made normalization impossible to turn off: the bars saw
    // `undefined`, drew the control OFF while the backend had it ON, and the
    // first interaction wrote the WRONG value back. Slint has no equivalent
    // hole because `SettingsState` is a global seeded at startup.
    publish_settings();

    // Same shape, same reason as the block above: the Recommendations cache
    // window is a fixed option list whose select has to open on the STORED
    // entry. Published once here, at shell entry, because that is the first
    // point where the per-user prefs directory is known — the bridge's own
    // default is just the store's default (48 h) and would silently misreport
    // a user who chose something else until they changed it again.
    recommendations_qt::publish_cache_ttl_index();

    // Restore the persisted player volume so audio starts at the SAVED level
    // (1:1 with qbz/src/main.rs:220-227). Without it every launch started at
    // 100% — on an exclusive-mode DAC that is not a cosmetic default.
    let restored = crate::settings_qt::read_pref_f32("volume")
        .unwrap_or(1.0)
        .clamp(0.0, 1.0);
    // ...and seed the UI model with the SAME value, or the engine and the
    // slider disagree from the first frame.
    //
    // This line used to be absent, under a comment claiming "the poll loop
    // mirrors it onto the bar's slider from the engine, so nothing else has to
    // publish it". No such mirror exists on the local path:
    // `now_playing::set_volume` has exactly two callers — the Cast echo
    // (`cast_qt.rs:783`) and the QConnect PEER branch
    // (`playback_qt.rs:2167`) — and neither runs in a normal local session. So
    // the model kept its idle default of `volume: 1.0` (`now_playing.rs:94`,
    // "no track, full volume") and every launch drew the slider at 100 % while
    // the audio was correctly at the saved level. Owner-reported 2026-08-14.
    //
    // One call covers every surface: the NPB (`PlayerBar.qml:585`), the small
    // bar, the immersive bar (`ImmersivePlayerBar.qml:282`) and the miniplayer
    // all bind `QbzPlayer.npVolume`, which `now_playing::publish` writes.
    // (The kiosk shell has no volume control at all — checked, not assumed.)
    //
    // Ordering is safe: `set_volume` only mutates the model and republishes, so
    // it does not race the async engine call above.
    crate::now_playing::set_volume(restored);

    // Qobuz Connect service wiring (2026-08-01 contract §11.5), in order.
    // This fn is MULTI-ENTRY (login, session restore AND start_offline all
    // land here), and every caller runs on the tokio runtime, so
    // `Handle::current()` is valid.
    // 1. The service singleton — without it `service()` is None and every
    //    arm no-ops silently. OnceLock-idempotent, so the multi-entry seam
    //    calls it every time.
    let qconnect_service = qconnect_qt::init_service(app());
    let tokio_handle = tokio::runtime::Handle::current();
    {
        // 2. The offline force-disconnect watcher — ONCE per process: the fn
        //    carries NO idempotency guard of its own (unlike the auto-connect
        //    below), so a naive re-spawn per shell entry would leak one
        //    permanent watcher task per entry (double force-disconnect,
        //    double badge flip).
        use std::sync::atomic::{AtomicBool, Ordering};
        static OFFLINE_WATCHER_SPAWNED: AtomicBool = AtomicBool::new(false);
        if !OFFLINE_WATCHER_SPAWNED.swap(true, Ordering::SeqCst) {
            qconnect_service.spawn_offline_force_disconnect(&tokio_handle);
        }
    }
    // Revalidate hardware volume first. When a compatible ALSA mixer exists,
    // the probe samples its physical level and seeds SharedState without a
    // write; restoring the persisted slider afterward would otherwise move
    // that mixer at launch. Without hardware volume, restore the saved level
    // and then enforce unity for the protected ALSA-direct path. Sequencing
    // these decisions in one task prevents an asynchronous restore from
    // landing after the bit-perfect correction.
    let rt = app();
    spawn(async move {
        let hardware_level = settings_qt::reconcile_alsa_hardware_volume(&rt).await;
        if hardware_level.is_none() {
            playback_qt::set_volume(&rt, restored).await;
        }
        settings_qt::maybe_force_bitperfect_volume(&rt).await;
    });
    // 3. Startup auto-connect — gated on NOT offline: the offline shell entry
    //    never auto-connects, and calling the spawn there would burn its
    //    internal once-per-process FIRED latch before a real online entry
    //    could use it. (The task itself also re-checks offline inside its
    //    retry loop and bails silently.)
    if !offline_fwd::engine().status().is_offline() {
        qconnect_qt::spawn_startup_auto_connect(&tokio_handle);
    }
}

/// Post-login UI state: the shared session entry, then the session header,
/// has-previous-session, cleared login errors/phase and the screen switch.
fn enter_shell(session: auth_qt::SessionInfo) {
    integrations_qt::set_qobuz_authenticated(true);
    on_session_entered();
    session_bridge::ui(move |mut b| {
        b.as_mut()
            .set_session_user_name(QString::from(session.display_name.as_str()));
        b.as_mut().set_session_subscription(QString::from(
            crate::auth_qt::display_subscription(&session.subscription).as_str(),
        ));
        b.as_mut().set_has_previous_session(true);
        b.as_mut().set_login_error(QString::from(""));
        b.as_mut().set_restore_error(QString::from(""));
        b.as_mut().set_login_phase(0);
        // Kiosk profile (2026-08-02 kiosk-port contract §8.2): the post-login
        // screen is the kiosk touch shell when the profile is active. The Qt
        // spelling of `resolved_shell_screen()` (Slint main.rs:7869-7876).
        b.as_mut()
            .set_screen(QString::from(kiosk_profile_qt::shell_screen()));
    });
    // Drain a cold-start launcher URL only after authenticated session entry;
    // the resolver's navigation requires the Qobuz API.
    deep_link_qt::set_online_session(true);
}

/// Login screen primary button / recovery banner: the system-browser OAuth
/// flow on a background task; phases and results hop back to Qt.
pub(crate) fn start_login() {
    {
        let guard = LOGIN_TASK.lock().unwrap();
        if guard.is_some() {
            log::warn!("[qbz-qt] login already in progress, ignoring");
            return;
        }
    }
    session_bridge::ui(|mut b| {
        b.as_mut().set_login_error(QString::from(""));
        b.as_mut().set_login_phase(1);
    });

    let runtime = app();
    let handle = TOKIO.get().unwrap().spawn(async move {
        let result = auth_qt::login_via_system_browser(&runtime, |phase| {
            let value = match phase {
                auth_qt::LoginPhase::WaitingForBrowser => 1,
                auth_qt::LoginPhase::Authenticating => 2,
            };
            session_bridge::ui(move |mut b| b.as_mut().set_login_phase(value));
        })
        .await;
        LOGIN_TASK.lock().unwrap().take();
        match result {
            Ok(session) => enter_shell(session),
            Err(e) => session_bridge::ui(move |mut b| {
                b.as_mut().set_login_phase(0);
                b.as_mut().set_login_error(QString::from(e.as_str()));
            }),
        }
    });
    *LOGIN_TASK.lock().unwrap() = Some(handle);
}

/// Cancel link (phase 1): abort the in-flight OAuth and return to idle.
pub(crate) fn cancel_login() {
    if let Some(task) = LOGIN_TASK.lock().unwrap().take() {
        task.abort();
    }
    session_bridge::ui(|mut b| b.as_mut().set_login_phase(0));
}

/// "Start offline": unauthenticated offline session -> the shell.
///
/// It runs the SAME [`on_session_entered`] sequence a login does. It used to
/// run two lines of it (nav + now-playing), which is why an offline session
/// came up with no sidebar at all — no folders, no local playlists — and the
/// Refresh row was the only way back. See `on_session_entered`'s header.
pub(crate) fn start_offline() {
    deep_link_qt::set_online_session(false);
    let runtime = app();
    spawn(async move {
        match auth_qt::start_offline_session(&runtime).await {
            Ok(user_id) => {
                integrations_qt::set_qobuz_authenticated(false);
                let name = if user_id == 0 {
                    "Guest (offline)".to_string()
                } else {
                    format!("Offline (user {user_id})")
                };
                // The full session entry, in the same order the login path
                // uses it — NOT the two-line subset this used to run.
                on_session_entered();
                session_bridge::ui(move |mut b| {
                    b.as_mut()
                        .set_session_user_name(QString::from(name.as_str()));
                    b.as_mut().set_session_subscription(QString::from(""));
                    b.as_mut().set_login_error(QString::from(""));
                    b.as_mut().set_restore_error(QString::from(""));
                    b.as_mut().set_login_phase(0);
                    // Same profile resolution as the login path (§8.2).
                    b.as_mut()
                        .set_screen(QString::from(kiosk_profile_qt::shell_screen()));
                });
            }
            Err(e) => {
                log::error!("[qbz-qt] failed to enter offline mode: {e}");
                session_bridge::ui(move |mut b| {
                    b.as_mut().set_login_error(QString::from(e.as_str()));
                });
            }
        }
    });
}

/// Shell logout: token + session teardown, then back to the login screen.
pub(crate) fn do_logout() {
    // Flip this before the async teardown: a delayed scrobble firing anywhere
    // in that window must already obey the logged-out opt-out.
    integrations_qt::set_qobuz_authenticated(false);
    deep_link_qt::set_online_session(false);
    let runtime = app();
    spawn(async move {
        // QConnect may currently be rendering with delegated credentials and
        // an ephemeral guest queue. Withdraw LAN admission and restore the
        // owner authority before the owner token is removed or any session
        // state can be persisted under the next account.
        if let Some(service) = qconnect_qt::service() {
            match service.disconnect_safely().await {
                Ok(outcome) if outcome.authority_safe => {}
                Ok(_) => {
                    integrations_qt::set_qobuz_authenticated(true);
                    deep_link_qt::set_online_session(true);
                    crate::toast_qt::error(qbz_i18n::t(
                        "Qobuz Connect could not shut down safely. Quit and reopen QBZ, then try logging out again.",
                    ));
                    return;
                }
                Err(e) => {
                    log::error!("[qbz-qt] QConnect logout teardown failed: {e}");
                    integrations_qt::set_qobuz_authenticated(true);
                    deep_link_qt::set_online_session(true);
                    crate::toast_qt::error(qbz_i18n::t(
                        "Qobuz Connect could not shut down safely. Quit and reopen QBZ, then try logging out again.",
                    ));
                    return;
                }
            }
        }
        // A connected renderer keeps playing after the session ends unless it
        // is torn down explicitly (the cast service owns its own socket).
        cast_qt::service().shutdown().await;
        if let Err(e) = auth_qt::logout(&runtime).await {
            log::error!("[qbz-qt] logout failed: {e}");
        }
        // Integrations: drop the Discord presence so a signed-out session
        // stops advertising what was playing (the Slint discord_rpc::clear).
        integrations_qt::discord_clear();
        // The local-library documents are per-user; a later login must not
        // inherit the previous user's cached tree.
        local_library_qt::reset();
        // Purchases likewise: the list, the open album detail and the in-memory
        // download statuses are all scoped to the account that bought them.
        // Leaving them behind would show the next user someone else's library.
        purchases_qt::reset();
        // A later login must re-fetch Home + Library (new user, fresh data)
        // and re-warm the favourite-id cache that `auth_qt::logout` just
        // emptied — otherwise the next account's hearts stay blank until it
        // opens Library.
        //
        // The SIDEBAR latch is in that set too, for the same reason plus a
        // sharper one: the block below publishes `"[]"` into the tree, so a
        // latched `load_sidebar_once` leaves the next session staring at an
        // empty sidebar with no way back except the Refresh row
        // (owner-reproduced 2026-07-31). It is only resettable at all because
        // it now lives at module level — see SIDEBAR_LOADED.
        //
        // `on_session_entered` resets the same set: logout is NOT the only way
        // a session ends (offline -> login never passes through here).
        reset_session_latches();
        // The tree's own CACHE, not just the published document. It holds the
        // outgoing user's playlists, folders, folder membership, hidden set and
        // local rows, and NOTHING else clears it — every `publish_sidebar()`
        // rebuilds straight from it (the Playlist Manager's optimistic
        // move-to-folder does exactly that), so a leftover cache re-renders the
        // previous account's tree for the next one. Same class as the
        // `local_library_qt::reset()` above.
        sidebar_qt::teardown();
        session_bridge::ui(|mut b| {
            b.as_mut().set_session_user_name(QString::from(""));
            b.as_mut().set_session_subscription(QString::from(""));
            b.as_mut().set_login_error(QString::from(""));
            b.as_mut().set_restore_error(QString::from(""));
            b.as_mut().set_login_phase(0);
            b.as_mut().set_screen(QString::from("login"));
        });
        home_bridge::ui(|mut b| {
            b.as_mut().set_home_sections_json(QString::from("[]"));
            b.as_mut().set_home_error(QString::from(""));
            b.as_mut().set_home_loading(false);
        });
        library_bridge::ui(|mut b| {
            b.as_mut().set_library_json(QString::from("[]"));
            b.as_mut().set_library_counts_json(QString::from("{}"));
            b.as_mut().set_library_error(QString::from(""));
            b.as_mut().set_library_loading(false);
        });
        shell_bridge::ui(|mut b| {
            b.as_mut().set_sidebar_json(QString::from("[]"));
        });
    });
}

// ============================ Discover > Home ==============================

/// Fire the Home auto-load once per shell entry. Gated on the offline
/// engine: offline sessions show the OfflinePlaceholder instead of
/// fetching (the Qobuz gate would refuse the calls anyway).
fn load_home_once() {
    if *HOME_LOADED.lock().unwrap() {
        return;
    }
    if offline_fwd::engine().status().is_offline() {
        log::info!("[qbz-qt] home load skipped (offline session)");
        return;
    }
    *HOME_LOADED.lock().unwrap() = true;
    reload_home();
}

// ============================ Favourites cache =============================

/// Network refresh of the favourite-id cache, once per shell entry.
///
/// Skipped offline on purpose: the disk seed from `init_for_user` is the truth
/// there, and the Qobuz gate would refuse the calls anyway. The flag is reset
/// on logout alongside HOME_LOADED — `fav_cache_qt::teardown()` empties the
/// sets, so a second login in the same process MUST be able to warm again.
fn warm_favorites_once() {
    if offline_fwd::engine().status().is_offline() {
        log::info!("[qbz-qt] favorites cache warm skipped (offline session)");
        return;
    }
    {
        let mut guard = FAV_WARMED.lock().unwrap();
        if *guard {
            return;
        }
        *guard = true;
    }
    let runtime = app();
    spawn(async move {
        library_qt::warm_favorites_cache(&runtime).await;
    });
}

// ============================ Playback (phase 4) ===========================

// ============================ Sidebar + pins (phase 7) =====================

/// Load + publish the sidebar tree (session entry; idempotent per session).
///
/// It goes through [`reload_sidebar_including_local`], NOT [`reload_sidebar`],
/// and it has no offline early return of its own. A session that STARTS
/// offline used to get an entirely empty sidebar — no folders, no local
/// playlists — which is the whole library for a user with no Qobuz account.
/// `sidebar_qt::load` gates only its Qobuz fetch (preserving whatever the
/// cache already holds), so offline this reads folders + locals and publishes
/// them; online it is the same call it always was.
/// The latch is [`SIDEBAR_LOADED`], a MODULE-level static reset by `do_logout`
/// — see its declaration for the logout -> login bug a function-local one
/// caused.
fn load_sidebar_once() {
    {
        let mut guard = SIDEBAR_LOADED.lock().unwrap();
        if *guard {
            return;
        }
        *guard = true;
    }
    reload_sidebar_including_local();
}

/// Fetch playlists + folders and publish the flattened entries.
pub(crate) fn reload_sidebar() {
    if offline_fwd::engine().status().is_offline() {
        return;
    }
    let runtime = app();
    spawn(async move {
        sidebar_qt::load(&runtime).await;
        publish_sidebar();
    });
}

/// Same rebuild, but it also runs OFFLINE.
///
/// `reload_sidebar` bails while offline because its Qobuz fetch cannot
/// succeed. The sidebar's LOCAL playlists can still change in that state — the
/// picker's D8 branch creates one offline — and for a user with no Qobuz
/// account they are the entire sidebar, so a no-op there means the playlist
/// they just created never appears. `sidebar_qt::load` already reads the
/// locals independently of the Qobuz fetch and survives it failing or being
/// gate-refused, so there is nothing to guard against here beyond one wasted
/// (and already-handled) request.
pub(crate) fn reload_sidebar_including_local() {
    let runtime = app();
    spawn(async move {
        sidebar_qt::load(&runtime).await;
        publish_sidebar();
    });
}

/// Republish the flattened entries from the sidebar CACHE — no fetch, no DB
/// read.
///
/// `pub(crate)` since the Playlist Manager landed: its optimistic move-to-folder
/// patches `sidebar_qt::CACHE` in place and then republishes from it, which is
/// the only refresh that is correct offline AND does not cost a round trip
/// (contract D10).
pub(crate) fn publish_sidebar() {
    let deletion_revision = library_qt::playlist_deletion_revision();
    let insertion_revision = sidebar_qt::insertion_revision();
    let entries = sidebar_qt::rebuild();
    let json = serde_json::to_string(&entries).unwrap_or_else(|_| "[]".into());
    log::debug!(
        "[qbz-qt] sidebar published: {} entries ({} bytes)",
        entries.len(),
        json.len()
    );
    let (sort_by, sort_asc) = sidebar_qt::sort_state();
    shell_bridge::ui(move |mut b| {
        if deletion_revision != library_qt::playlist_deletion_revision()
            || insertion_revision != sidebar_qt::insertion_revision()
        {
            return;
        }
        b.as_mut().set_sidebar_json(QString::from(json.as_str()));
        b.as_mut()
            .set_sidebar_sort_by(QString::from(sort_by.as_str()));
        b.as_mut().set_sidebar_sort_asc(sort_asc);
    });
    // The collapsible My QBZ tree rides the SAME funnel: `load_sidebar_once()`
    // publishes the sidebar once per session (the reliable startup point, after
    // the QML singletons exist), and every sidebar mutation republishes here.
    // Gated by the opt-in inside, so a flat-sidebar profile pays nothing.
    // `myqbz_qt::reload_grids` still republishes it on mixtape/collection
    // mutations, which the playlist path here does not see.
    crate::myqbz_qt::publish_sidebar_tree();
}

pub(crate) fn sidebar_set_sort(option: &str) {
    sidebar_qt::set_sort(option);
    publish_sidebar();
}

pub(crate) fn sidebar_set_search(query: &str) {
    sidebar_qt::set_search(query);
    publish_sidebar();
}

pub(crate) fn sidebar_toggle_folder(id: &str) {
    sidebar_qt::toggle_folder(id);
    publish_sidebar();
}

/// Mini-rail folder flyout: publish that folder's playlists (contract §4.7).
///
/// This exists so the flyout does NOT have to force-expand the folder to see
/// its children. `sidebar_json` only carries a folder's rows while it is
/// EXPANDED, so the old path called `sidebar_toggle_folder` on the way in — a
/// persistent side effect (the folder stayed open once the sidebar was
/// re-opened) that the reference does not have. `sidebar_qt::folder_popup_rows`
/// answers from the session CACHE instead, which is also what makes this work
/// offline and on the Qt thread.
///
/// `count` is `rows.len()`, deliberately: the reference takes it from the entry
/// row, which is computed AFTER the playlist search filter that
/// `load_folder_popup` does not apply, so its header count and its list can
/// legitimately disagree. `folderName` is a best-effort cache read — the flyout
/// takes the name it renders from the clicked entry, synchronously, because
/// this document lands a later event-loop turn.
pub(crate) fn sidebar_open_folder_popup(folder_id: &str) {
    let rows = sidebar_qt::folder_popup_rows(folder_id);
    let count = rows.len();
    let name = sidebar_qt::folder_name(folder_id).unwrap_or_default();
    let doc = serde_json::json!({
        "folderId": folder_id,
        "folderName": name,
        "count": count,
        "rows": rows,
    });
    let json = doc.to_string();
    log::debug!("[qbz-qt] sidebar folder popup: {folder_id} ({count} rows)");
    shell_bridge::ui(move |mut b| {
        b.as_mut()
            .set_sidebar_folder_popup_json(QString::from(json.as_str()));
    });
}

/// Sidebar cover dispatch: plain url list (the tree's collage is
/// url-keyed). Disk hits emit immediately; misses download then emit.
pub(crate) fn sidebar_artwork_window(urls_json: String) {
    let urls: Vec<String> = serde_json::from_str(&urls_json).unwrap_or_default();
    log::debug!("[qbz-qt] sidebar artwork window: {} urls", urls.len());
    let mut missing: Vec<String> = Vec::new();
    for url in urls {
        let path = artwork_qt::cached_path(&url);
        if path.is_empty() {
            missing.push(url);
        } else {
            emit_library_artwork(url, path);
        }
    }
    if missing.is_empty() {
        return;
    }
    spawn(async move {
        artwork_qt::download_missing_progressive(missing, emit_library_artwork).await;
    });
}

// ============================ Album / Artist views (phase 8) ==============

/// Open the album detail view: push the nav entry, then fetch + publish.
pub(crate) fn open_album(album_id: String) {
    // Learn from results-page interactions too, not only from the
    // cortinilla. Self-gated on the Search view being current, so every other
    // caller of this router is unaffected.
    search_qt::record_page_interaction("album", &album_id, search_qt::InteractionAction::Open);
    // A LOCAL or Plex album id is a group key or a path, never a Qobuz catalog
    // id. Sending one to /album/get returns 404 and the view lands empty — the
    // mixed Library "All" feed hands both kinds to the same card, so the routing
    // has to happen HERE rather than in each call site.
    if library_qt::is_local_feed_id("album", &album_id) {
        nav_qt::record("localalbum");
        shell_bridge::ui(|mut b| b.as_mut().set_current_view(QString::from("localalbum")));
        local_bridge::open_album_by_id(album_id);
        return;
    }
    if offline_fwd::engine().status().is_offline() {
        return;
    }
    nav_qt::record_with("album", serde_json::json!({ "id": &album_id }));
    *LAST_DETAIL.lock().unwrap() = ("album".to_string(), album_id.clone());
    let runtime = app();
    album_bridge::ui(move |mut b| {
        // Clear the PREVIOUS album in the same hop that raises the loading
        // flag: without it the view renders the last album until the fetch
        // lands, so opening B after A showed A.
        b.as_mut().set_album_json(QString::from("{}"));
        b.as_mut().set_album_loading(true);
    });
    spawn(async move {
        match album_qt::load_album_view(&runtime, &album_id).await {
            Ok(json) => album_bridge::ui(move |mut b| {
                b.as_mut().set_album_json(QString::from(json.as_str()));
                b.as_mut().set_album_loading(false);
            }),
            Err(e) => {
                log::warn!("[qbz-qt] album view load failed: {e}");
                let gone = e == album_qt::ALBUM_GONE;
                album_qt::publish_unavailable(album_id, gone, e);
            }
        }
    });
}

/// Open the artist detail view: push the nav entry, then fetch + publish.
pub(crate) fn open_artist(artist_id: String) {
    // Learn from results-page interactions too, not only from the
    // cortinilla. Self-gated on the Search view being current, so every other
    // caller of this router is unaffected.
    search_qt::record_page_interaction("artist", &artist_id, search_qt::InteractionAction::Open);
    if offline_fwd::engine().status().is_offline() {
        return;
    }
    nav_qt::record_with("artist", serde_json::json!({ "id": &artist_id }));
    *LAST_DETAIL.lock().unwrap() = ("artist".to_string(), artist_id.clone());
    // Warm the Library feed in the background: the page's "In library" tab is
    // built off it, and on a cold session (or right after an account switch —
    // `reset_session_latches`) it was empty until the user happened to visit
    // Library, so the toggle never mounted. Idempotent through
    // `LIBRARY_LOADED`; with the feed warm this is a no-op, and when it lands
    // `reload_library` repaints the open page's rows.
    load_library_once();
    let runtime = app();
    artist_bridge::ui(move |mut b| {
        // Same as open_album: stale artist until the fetch lands otherwise.
        b.as_mut().set_artist_json(QString::from("{}"));
        b.as_mut().set_artist_loading(true);
    });
    spawn(async move {
        match artist_qt::load_artist_view(&runtime, &artist_id).await {
            Ok(json) => artist_bridge::ui(move |mut b| {
                b.as_mut().set_artist_json(QString::from(json.as_str()));
                b.as_mut().set_artist_loading(false);
            }),
            Err(e) => {
                log::warn!("[qbz-qt] artist view load failed: {e}");
                artist_bridge::ui(move |mut b| b.as_mut().set_artist_loading(false));
            }
        }
    });
}

/// ArtistView "Load more" for one releases bucket (page 20, has_more).
pub(crate) fn load_release_section(artist_id: String, release_type: String, offset: i32) {
    let runtime = app();
    spawn(async move {
        match artist_qt::load_release_page(
            &runtime,
            &artist_id,
            &release_type,
            offset.max(0) as u32,
        )
        .await
        {
            Ok(Some((cards, has_more, request))) => {
                // The user may have opened ANOTHER artist while this page was
                // in flight. `merge_release_page` already dropped the stash
                // merge in that case (its id guard), but the signal carries no
                // artist id, and ArtistView.qml keys its append overlay by
                // release_type alone — emitting here would graft this artist's
                // page onto the new artist's same-named bucket. Same test the
                // merge used (artist_qt::stash_is_for), applied to the second
                // leg of the same delivery.
                if !artist_qt::release_request_is_current(&artist_id, &release_type, &request) {
                    log::info!(
                        "[qbz-qt] dropping stale release page ({release_type}): artist changed"
                    );
                    return;
                }
                let json = serde_json::to_string(&cards).unwrap_or_else(|_| "[]".into());
                artist_bridge::ui(move |mut b| {
                    if !artist_qt::release_request_is_current(&artist_id, &release_type, &request) { return; }
                    b.as_mut().release_section_ready(
                        QString::from(release_type.as_str()),
                        QString::from(json.as_str()),
                        has_more,
                    );
                });
            }
            Ok(None) => {},
            Err(e) => log::warn!("[qbz-qt] release page load failed: {e}"),
        }
    });
}

/// AlbumCard ⋯ menu: Play next / Add to queue.
pub(crate) fn enqueue_album(album_id: String, mode: String) {
    let runtime = app();
    spawn(async move {
        if let Err(e) = playback_qt::enqueue_album(&runtime, &album_id, &mode).await {
            log::error!("[qbz-qt] enqueue_album failed: {e}");
        }
    });
}

/// Card / detail-header pin badge: mutate the store, then fan the result out
/// — the ADR-006 per-user stores have no change-notify, so the mutation site
/// owns the fan-out (Slint's `on_toggle_pin` does the same thing: badge walk
/// over the live models, then `pinned_section::rebuild_pinned`).
///
/// FIVE consumers, and only ONE of them re-renders anything:
///  - `pin-changed` (`{kind}:{id}`) — the badge walk. Every AlbumCard /
///    ArtistCard / PlaylistCard on screen listens for its own key and flips
///    its glyph in place, and the Library All feed patches its own row. No
///    model is replaced, so no delegate is torn down.
///  - `home_qt::apply_pin_change` — patches the cached candidate rows (for
///    the NEXT republish, whatever causes it) and rebuilds the Pinned rail
///    on its own `pinnedJson` property. The three tab documents are left
///    alone.
///  - `recommendations_qt::apply_pin_change` — same shape for that tab's
///    separate cache; badge-only (it has no pinned rail).
///  - `search_qt::apply_pin_change` — same again. Search NEEDS it because its
///    cached page is re-published routinely (the artwork pass after `submit`,
///    plus `tab_changed` / `load_more` / `filter_changed`), and each of those
///    swaps the model out from under the card: without the patch the stale
///    `isPinned` in the cache silently reverted the user's flip a second after
///    the click, whenever a cover happened to land.
///  - `library_qt::apply_pin_change` — patches the cached merged feed. Only
///    re-serialized by a full `reload_library` today, so it is the cheap
///    insurance rather than a live bug.
///
/// EVERY consumer here is a cache patch or an in-place badge signal. The rule
/// this encodes: a per-click mutation must never republish a document. See
/// `home_qt::apply_pin_change`'s docs for what republishing cost.
pub(crate) fn toggle_pin(
    kind: String,
    id: String,
    title: String,
    subtitle: String,
    artwork_url: String,
) {
    if let Some(value) = sidebar_qt::toggle_pin(&kind, &id, &title, &subtitle, &artwork_url) {
        let key = format!("{kind}:{id}");
        library_bridge::ui(move |mut b| {
            b.as_mut().pin_changed(QString::from(key.as_str()), value);
        });
        home_qt::apply_pin_change(&kind, &id, value);
        recommendations_qt::apply_pin_change(&kind, &id, value);
        search_qt::apply_pin_change(&kind, &id, value);
        library_qt::apply_pin_change(&kind, &id, value);
    }
}

// ============================ Lyrics panel (phase 9) ======================

/// NPB lyrics button / panel close: toggle the column section and — like
/// LyricsState.panel-opened — re-request the current track's lyrics on
/// open (cached docs return immediately). The static is the source of
/// truth (the ui() hop is queued, not synchronous).
static LYRICS_OPEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub(crate) fn toggle_lyrics() {
    let open = !LYRICS_OPEN.swap(
        !LYRICS_OPEN.load(std::sync::atomic::Ordering::SeqCst),
        std::sync::atomic::Ordering::SeqCst,
    );
    shell_bridge::ui(move |mut b| {
        b.as_mut().set_lyrics_open(open);
    });
    if open {
        let runtime = app();
        spawn(async move {
            if let Some(track) = runtime.core().current_track().await {
                lyrics_qt::load_for_track(&runtime, &track).await;
            }
        });
    }
}

// ============================ Queue panel (phase 9) =======================

pub(crate) fn queue_set_page(page: i32) {
    queue_qt::set_page(page);
    let runtime = app();
    spawn(async move { queue_qt::publish(&runtime).await });
}

pub(crate) fn queue_set_search(query: String) {
    queue_qt::set_search(&query);
    let runtime = app();
    spawn(async move { queue_qt::publish(&runtime).await });
}

pub(crate) fn queue_play_upcoming(index: i32) {
    let runtime = app();
    spawn(async move { queue_qt::play_upcoming(&runtime, index.max(0) as usize).await });
}

/// Immersive coverflow / up-next rows: QUEUE-WIDE 0-based upcoming index
/// (contract §4.4) — bypasses the queue panel's page/search VIEW.
pub(crate) fn queue_play_upcoming_flat(index: i32) {
    let runtime = app();
    spawn(async move { queue_qt::play_upcoming_flat(&runtime, index.max(0) as usize).await });
}

pub(crate) fn queue_extended_opened() {
    queue_qt::extended_opened();
    let runtime = app();
    spawn(async move { queue_qt::publish(&runtime).await });
}

pub(crate) fn queue_extended_play(phase: String, index: i32, expected_id: String) {
    let Ok(track_id) = expected_id.parse::<u64>() else {
        return;
    };
    let runtime = app();
    spawn(async move {
        queue_qt::play_extended(&runtime, &phase, index.max(0) as usize, track_id).await
    });
}

pub(crate) fn queue_extended_drop(
    phase: String,
    index: i32,
    expected_id: String,
    target_phase: String,
    slot: i32,
) {
    let Ok(track_id) = expected_id.parse::<u64>() else {
        return;
    };
    let runtime = app();
    spawn(async move {
        queue_qt::drop_extended(
            &runtime,
            &phase,
            index.max(0) as usize,
            track_id,
            &target_phase,
            slot.max(0) as usize,
        )
        .await
    });
}

pub(crate) fn queue_remove_upcoming_flat(index: i32) {
    let runtime = app();
    spawn(async move { queue_qt::remove_upcoming_flat(&runtime, index.max(0) as usize).await });
}

pub(crate) fn queue_remove_all_after_flat(index: i32) {
    let runtime = app();
    spawn(async move { queue_qt::remove_all_after_flat(&runtime, index.max(0) as usize).await });
}

pub(crate) fn queue_remove_upcoming(index: i32) {
    let runtime = app();
    spawn(async move { queue_qt::remove_upcoming(&runtime, index.max(0) as usize).await });
}

pub(crate) fn queue_remove_all_after(index: i32) {
    let runtime = app();
    spawn(async move { queue_qt::remove_all_after(&runtime, index.max(0) as usize).await });
}

pub(crate) fn queue_move_track(from: i32, to: i32) {
    let runtime = app();
    spawn(
        async move { queue_qt::move_track(&runtime, from.max(0) as usize, to.max(0) as usize).await },
    );
}

pub(crate) fn queue_play_history(index: i32) {
    let runtime = app();
    spawn(async move { queue_qt::play_history(&runtime, index.max(0) as usize).await });
}

pub(crate) fn queue_clear() {
    let runtime = app();
    spawn(async move { queue_qt::clear_queue(&runtime).await });
}

pub(crate) fn queue_toggle_favorite(kind: String, id: String) {
    let runtime = app();
    spawn(async move { queue_qt::toggle_favorite(&runtime, &kind, &id).await });
}

pub(crate) fn queue_toggle_stop_after(id: String) {
    let runtime = app();
    spawn(async move { queue_qt::toggle_stop_after(&runtime, &id).await });
}

pub(crate) fn queue_toggle_infinite_play() {
    let runtime = app();
    spawn(async move { queue_qt::toggle_infinite_play(&runtime).await });
}

pub(crate) fn queue_save_as_playlist() {
    let runtime = app();
    spawn(async move { queue_qt::save_as_playlist(&runtime).await });
}

pub(crate) fn queue_add_to_playlist(index: i32) {
    let runtime = app();
    spawn(async move { queue_qt::add_to_playlist(&runtime, index.max(0) as usize).await });
}

pub(crate) fn queue_panel_opened() {
    let runtime = app();
    spawn(async move { queue_qt::panel_opened(&runtime).await });
}

/// AlbumView header Shuffle.
/// Artist page album-section split button: "Play all" / "Play selected"
/// over a JSON array of album ids, in the section's sort order.
pub(crate) fn play_albums(ids_json: String) {
    let ids: Vec<String> = serde_json::from_str(&ids_json).unwrap_or_default();
    if ids.is_empty() {
        return;
    }
    let runtime = app();
    spawn(async move {
        if let Err(e) = playback_qt::play_albums(&runtime, &ids).await {
            log::error!("[qbz-qt] play_albums failed: {e}");
        }
    });
}

pub(crate) fn play_album_shuffled(album_id: String) {
    let runtime = app();
    spawn(async move {
        if let Err(e) = playback_qt::play_album_shuffled(&runtime, &album_id).await {
            log::error!("[qbz-qt] play_album_shuffled failed: {e}");
        }
    });
}

/// AlbumView row play (album from the clicked track).
pub(crate) fn play_album_from_track(album_id: String, track_id: u64) {
    let runtime = app();
    spawn(async move {
        if let Err(e) = playback_qt::play_album_from_track(&runtime, &album_id, track_id).await {
            log::error!("[qbz-qt] play_album_from_track failed: {e}");
        }
    });
}

/// AlbumView row "Play next" / "Add to queue".
pub(crate) fn enqueue_album_track(album_id: String, track_id: u64, mode: String) {
    let runtime = app();
    spawn(async move {
        if let Err(e) = playback_qt::enqueue_album_track(&runtime, &album_id, track_id, &mode).await
        {
            log::error!("[qbz-qt] enqueue_album_track failed: {e}");
        }
    });
}

/// ArtistView Popular Tracks row play (the whole list as the queue).
pub(crate) fn play_artist_track(track_id: u64) {
    let runtime = app();
    spawn(async move {
        let (queue, start) = artist_qt::top_queue(Some(track_id));
        if let Err(e) = playback_qt::play_track_list(&runtime, queue, start, false).await {
            log::error!("[qbz-qt] play_artist_track failed: {e}");
        }
    });
}

/// ArtistView "Play all" / "Shuffle all".
pub(crate) fn play_artist_top(shuffle: bool) {
    let runtime = app();
    spawn(async move {
        let (queue, start) = artist_qt::top_queue(None);
        if let Err(e) = playback_qt::play_track_list(&runtime, queue, start, shuffle).await {
            log::error!("[qbz-qt] play_artist_top failed: {e}");
        }
    });
}

/// ArtistView ⋯ "Play all next" ("next") / "Add all to queue" ("queue").
///
/// Both rows land here now. The append-only body this shim used to hold was
/// the reason `playback_qt::enqueue_artist_top_mode` — written for exactly
/// this, and the reference's own shape (`main.rs:15301`,
/// `enqueue_artist_top_selected(.., next: true)`) — had no caller at all.
pub(crate) fn enqueue_artist_top(mode: String) {
    let runtime = app();
    spawn(async move {
        if let Err(e) = playback_qt::enqueue_artist_top_mode(&runtime, &mode).await {
            log::error!("[qbz-qt] enqueue_artist_top ({mode}) failed: {e}");
        }
    });
}

/// Track-row click (Library tracks): one-track queue through the core.
pub(crate) fn play_track(track_id: u64) {
    search_qt::record_page_interaction(
        "track",
        &track_id.to_string(),
        search_qt::InteractionAction::Play,
    );
    now_playing::begin_loading();
    let runtime = app();
    spawn(async move {
        if let Err(e) = playback_qt::play_single_track(&runtime, track_id).await {
            log::error!("[qbz-qt] play_track failed: {e}");
        }
    });
}

/// `play_track` with the row's ORIGIN attached — the single router for rails
/// that can show non-Qobuz tracks (Home's Recently-Played, and any future
/// mixed feed). Qobuz keeps the existing path byte for byte; everything else
/// resolves through its own source and never touches the catalog.
pub(crate) fn play_track_from(track_id: u64, source: String) {
    if source.is_empty() || source == "qobuz" {
        play_track(track_id);
        return;
    }
    search_qt::record_page_interaction(
        "track",
        &track_id.to_string(),
        search_qt::InteractionAction::Play,
    );
    // Spinner ON at DISPATCH: everything after this is async, and a local file
    // read or a Plex part fetch can take seconds with nothing on screen.
    now_playing::begin_loading();
    let runtime = app();
    spawn(async move {
        // No Qobuz fallback on failure, deliberately: that is the 404 this
        // router exists to remove, and guessing another row is the failure the
        // cortinilla refuses by design (search_qt.rs:1172).
        if !local_playback::play_single_from_source(&runtime, track_id, &source).await {
            log::error!("[qbz-qt] play_track_from: {source} track {track_id} did not play");
        }
    });
}

// ============================ Playlist view + DnD (phase 17) ==============

/// Open the playlist detail view (sidebar row / playlist card click).
pub(crate) fn open_playlist(playlist_id: String) {
    // Learn from results-page interactions too, not only from the
    // cortinilla. Self-gated on the Search view being current, so every other
    // caller of this router is unaffected.
    search_qt::record_page_interaction(
        "playlist",
        &playlist_id,
        search_qt::InteractionAction::Open,
    );
    // Drop the previous detail's "mixed" latch before either arm runs. `load`
    // sets it authoritatively at the end, but between here and there sits a
    // network fetch, and a remove clicked inside that window would otherwise
    // route by the shape of the page the user just left.
    playlist_qt::clear_mixed();
    // LOCAL playlists (`local:<uuid>`) route to their own loader and open
    // REGARDLESS of connectivity. They are the whole feature for people using
    // QBZ as a player without Qobuz — refusing them while offline would gate
    // local files behind a network the user does not have.
    if local_playlist_qt::is_local_id(&playlist_id) {
        nav_qt::record_with("playlist", serde_json::json!({ "id": &playlist_id }));
        ui(|mut b| b.as_mut().set_playlist_json(QString::from("{}")));
        let runtime = app();
        spawn(async move {
            if !local_playlist_qt::load(&runtime, &playlist_id).await {
                log::warn!("[qbz-qt] local playlist {playlist_id} load failed");
            }
        });
        return;
    }
    let Some(pid) = playlist_id.parse::<u64>().ok() else {
        log::warn!("[qbz-qt] open_playlist: invalid id {playlist_id}");
        return;
    };
    nav_qt::record_with("playlist", serde_json::json!({ "id": &playlist_id }));
    // Clear the previous playlist before the fetch — same stale-render as the
    // album and artist views had.
    ui(|mut b| b.as_mut().set_playlist_json(QString::from("{}")));
    // A Qobuz detail must not inherit the previous LOCAL detail's snapshot, or
    // its rows would resolve against the wrong playlist's queue.
    local_playlist_qt::clear_open_snapshot();
    if offline_fwd::engine().status().is_offline() {
        spawn(async move {
            if !local_playlist_qt::load_qobuz_offline(pid).await {
                log::warn!("[qbz-qt] offline playlist {pid} load failed");
            }
        });
        return;
    }
    let runtime = app();
    spawn(async move {
        if let Err(e) = playlist_qt::load(&runtime, pid).await {
            log::warn!("[qbz-qt] playlist load failed: {e}");
        }
    });
}

/// Reload the currently visible playlist without recording another
/// navigation entry or search interaction. Cover edits can originate from the
/// detail header, the shared editor in Playlist Manager, or the sidebar; only
/// the first case should refresh this page, and none should look like a fresh
/// user navigation.
pub(crate) fn reload_playlist_if_showing(playlist_id: String) {
    if nav_qt::current_view() != "playlist"
        || playlist_qt::open_doc_id().as_deref() != Some(playlist_id.as_str())
    {
        return;
    }

    if local_playlist_qt::is_local_id(&playlist_id) {
        let runtime = app();
        spawn(async move {
            if !local_playlist_qt::load(&runtime, &playlist_id).await {
                log::warn!("[qbz-qt] local playlist {playlist_id} reload failed");
            }
        });
        return;
    }

    let Ok(pid) = playlist_id.parse::<u64>() else {
        return;
    };
    if offline_fwd::engine().status().is_offline() {
        spawn(async move {
            if !local_playlist_qt::load_qobuz_offline(pid).await {
                log::warn!("[qbz-qt] offline playlist {pid} reload failed");
            }
        });
        return;
    }

    let runtime = app();
    spawn(async move {
        if let Err(e) = playlist_qt::load(&runtime, pid).await {
            log::warn!("[qbz-qt] playlist reload failed: {e}");
        }
    });
}

// ======================= MyQBZ (crate-level forwards) =====================
//
// W6: the ONLY crate-root forward the MyQBZ domain needs. Everything else the
// three bridges call, they call straight at its controller module
// (`crate::myqbz_qt::…`, `crate::myqbz_builder_qt::…`) — a thin forward per
// invokable would be 48 dead one-liners.
//
// This one exists because the CONTROLLERS need it, not the bridges: both
// `myqbz_qt::create_submit` (Create modal) and `myqbz_builder_qt::create`
// (Artist-Collection builder) navigate to the collection they just created,
// and neither should reach into a sibling controller to do it.

/// Open a mixtape/collection detail page by id — the shared "created, now go
/// there" hop. `myqbz_detail_qt::open` records the `"mixtapedetail"` route
/// itself, so this deliberately does NOT call `navigate_to` (which would
/// record a second entry and clear `LAST_DETAIL` twice).
pub(crate) fn myqbz_open_detail(id: String) {
    myqbz_detail_qt::open(id);
}

pub(crate) fn playlist_play_all() {
    let runtime = app();
    spawn(async move {
        // A LOCAL detail plays from its OWN resolved snapshot: its rows are a
        // mix of catalog, file and Plex tracks that the Qobuz play-all path
        // cannot build, and an offline-only one has to stamp the queue.
        if local_playlist_qt::open_id().is_some() {
            local_playlist_qt::play(&runtime, "").await;
            return;
        }
        if let Err(e) = playlist_qt::play_all(&runtime).await {
            log::error!("[qbz-qt] playlist play-all failed: {e}");
        }
    });
}

pub(crate) fn playlist_shuffle() {
    let runtime = app();
    spawn(async move {
        // The local guard `playlist_play_all` carries. The MODE is raised so
        // what follows this list stays shuffled, but the list itself is mixed
        // by `play_shuffled` — the flag alone would start on the playlist's #1
        // track every time (owner ruling 2026-08-01: every shuffle must be
        // genuinely random).
        if local_playlist_qt::open_id().is_some() {
            let Some(_owner_action) = playback_qt::begin_owner_action() else {
                return;
            };
            runtime.core().set_shuffle(true).await;
            now_playing::set_shuffle(true);
            local_playlist_qt::play_shuffled(&runtime).await;
            return;
        }
        if let Err(e) = playlist_qt::play_shuffled(&runtime).await {
            log::error!("[qbz-qt] playlist shuffle failed: {e}");
        }
    });
}

pub(crate) fn playlist_enqueue_all(mode: String) {
    let runtime = app();
    spawn(async move {
        // Same source-aware routing as header Play / Shuffle: a first-class
        // local detail and a mixed Qobuz detail both carry an already-resolved
        // queue snapshot whose local/Plex rows cannot be rebuilt by the
        // catalog-id-only card seam.
        let result = if local_playlist_qt::open_id().is_some() {
            local_playlist_qt::enqueue_all(&runtime, &mode).await
        } else {
            playlist_qt::enqueue_all(&runtime, &mode).await
        };
        if let Err(e) = result {
            log::error!("[qbz-qt] playlist enqueue-all ({mode}) failed: {e}");
        }
    });
}

pub(crate) fn playlist_toggle_follow() {
    let runtime = app();
    spawn(async move { playlist_qt::toggle_follow(&runtime).await });
}

pub(crate) fn playlist_copy() {
    let runtime = app();
    spawn(async move { playlist_qt::copy_playlist(&runtime).await });
}

pub(crate) fn playlist_rename(name: String) {
    let runtime = app();
    spawn(async move {
        if let Err(e) = playlist_qt::rename(&runtime, &name).await {
            log::error!("[qbz-qt] playlist rename failed: {e}");
        }
    });
}

pub(crate) fn playlist_delete() {
    let runtime = app();
    spawn(async move {
        if let Err(e) = playlist_qt::delete_playlist(&runtime).await {
            log::error!("[qbz-qt] playlist delete failed: {e}");
        }
    });
}

pub(crate) fn playlist_play_track(track_id: String) {
    let runtime = app();
    spawn(async move {
        // Same guard as `playlist_play_all` above, and for the same reason: a
        // LOCAL detail plays from its OWN resolved snapshot. Without it a row
        // click went through `playlist_qt::play_track` -> `current_queue()` ->
        // `row_to_queue`, which types EVERY row as Qobuz (`is_local: false`,
        // `source: "qobuz"`) — so a local file was queued as a catalog track
        // and `governed` came out true on the quality seed. `local_playlist_qt
        // ::play` matches on the same display id the rows carry
        // (`row_to_display` sets `id: queue.id.to_string()`), Qobuz rows
        // included, so the whole mixed detail is served by one path.
        if local_playlist_qt::open_id().is_some() {
            local_playlist_qt::play(&runtime, &track_id).await;
            return;
        }
        if let Err(e) = playlist_qt::play_track(&runtime, &track_id).await {
            log::error!("[qbz-qt] playlist row play failed: {e}");
        }
    });
}

pub(crate) fn playlist_enqueue_track(track_id: String, mode: String) {
    let runtime = app();
    spawn(async move {
        if let Err(e) = playlist_qt::enqueue_track(&runtime, &track_id, &mode).await {
            log::error!("[qbz-qt] playlist row enqueue ({mode}) failed: {e}");
        }
    });
}

/// Row ⋯ "Remove from playlist", BY DISPLAY ROW ID.
///
/// The id is a string because a LOCAL playlist's rows are not all catalog
/// tracks: a local-file row's id is a library row id, an unresolved one is
/// `"plex:<key>"` or a bare path. The Qobuz arm parses it back to the
/// membership row id, which for a Qobuz playlist IS the catalog id
/// (`playlist_qt.rs:364` sets `playlist_track_id: track.id`).
///
/// Routing by OPEN DETAIL, not by the id's shape: `local_playlist_qt::open_id`
/// is `Some` only while a `local:` detail is the open page (`open_playlist`
/// clears the snapshot before a Qobuz load), and it is the same test
/// `playlist_play_all` already routes on.
pub(crate) fn playlist_remove_track(row_id: String) {
    let runtime = app();
    spawn(async move {
        // `local_detail_open`, NOT `open_id().is_some()`: since the mixed
        // merge landed, a Qobuz detail with sidecar rows adopts that same
        // snapshot, and routing on its mere presence would send this remove
        // into the LOCAL playlist repo — which holds no row for a Qobuz
        // playlist id, so the click would render as done and write nothing.
        if local_playlist_qt::local_detail_open() {
            local_playlist_qt::remove_row(&runtime, &row_id).await;
            return;
        }
        // A sidecar row of a MIXED Qobuz playlist: it comes out of
        // `library.db`, not out of the Qobuz membership. Returns false when the
        // row turns out to be an ordinary Qobuz one, so this falls through.
        if playlist_qt::is_mixed() && playlist_qt::remove_sidecar_row(&runtime, &row_id).await {
            return;
        }
        let Ok(playlist_track_id) = row_id.parse::<u64>() else {
            log::warn!("[qbz-qt] playlist remove: non-numeric row id {row_id}");
            return;
        };
        playlist_qt::remove_track(&runtime, playlist_track_id).await
    });
}

/// Multi-select "Remove from playlist": the three arms of
/// `playlist_remove_track` over every selected row — the local repo rows in
/// one pass, the sidecar rows one by one (each is its own table write), the
/// Qobuz memberships in ONE API call.
pub(crate) fn playlist_remove_tracks(ids_json: String) {
    let ids: Vec<String> = serde_json::from_str(&ids_json).unwrap_or_default();
    if ids.is_empty() {
        return;
    }
    let runtime = app();
    spawn(async move {
        if local_playlist_qt::local_detail_open() {
            local_playlist_qt::remove_rows(&runtime, &ids).await;
            return;
        }
        let mut catalog: Vec<u64> = Vec::new();
        for row_id in &ids {
            if playlist_qt::is_mixed() && playlist_qt::remove_sidecar_row(&runtime, row_id).await {
                continue;
            }
            match row_id.parse::<u64>() {
                Ok(id) => catalog.push(id),
                Err(_) => log::warn!("[qbz-qt] playlist bulk remove: non-numeric row id {row_id}"),
            }
        }
        if !catalog.is_empty() {
            playlist_qt::remove_tracks(&runtime, &catalog).await;
        }
    });
}

/// Drag-reorder drop: move the visible row `from` to insertion slot `slot`.
///
/// Routes like the chevrons below: a LOCAL playlist writes the repo `position`
/// order directly (no sidecar — the repo order IS the order), a Qobuz one
/// rebuilds the custom-order sidecar. Same split as the reference
/// (`main.rs:20815` `on_reorder_track`).
pub(crate) fn playlist_reorder(from: i32, slot: i32) {
    if from < 0 || slot < 0 || slot == from || slot == from + 1 {
        return;
    }
    if local_playlist_qt::local_detail_open() {
        let runtime = app();
        spawn(async move {
            local_playlist_qt::reorder_row(&runtime, from as usize, slot as usize).await;
        });
        return;
    }
    // A MIXED Qobuz playlist has no reorder yet, and the honest answer is to
    // do nothing rather than to corrupt the page: the custom-order sidecar
    // keys by `u64` catalog id, and a local row's display id is a library
    // rowid in the same numeric space — writing one would collide with a real
    // track and reorder the WRONG row on the next load. The view hides the
    // affordance (PlaylistView.qml gates `canReorder` on `!doc.isMixed`); this
    // is the guard for every other way in.
    if playlist_qt::is_mixed() {
        log::info!("[qbz-qt] playlist reorder ignored: mixed playlist");
        return;
    }
    playlist_qt::reorder_track(from as usize, slot as usize);
}

/// Per-row reorder chevrons: -1 = up, +1 = down. Same routing as the drag.
pub(crate) fn playlist_move_row(row_id: String, delta: i32) {
    if delta == 0 {
        return;
    }
    if local_playlist_qt::local_detail_open() {
        let runtime = app();
        spawn(async move {
            local_playlist_qt::move_row(&runtime, &row_id, delta).await;
        });
        return;
    }
    // Same guard, same reason as the drag above.
    if playlist_qt::is_mixed() {
        log::info!("[qbz-qt] playlist move-row ignored: mixed playlist");
        return;
    }
    playlist_qt::move_row(&row_id, delta);
}

// --- Drag & drop (DragState + DragActions, state.slint parity) -----------
//
// The dragged payload: Qobuz catalog ids only for the POC (the Slint's
// DragTrack enum also carries local-library rows and Plex keys into
// sidecar writes — out of scope, see module POC-NOTEs).
static DRAGGED: Mutex<Vec<u64>> = Mutex::new(Vec::new());
/// LOCAL-mode drag payload: Local Library row ids, resolved to source-aware
/// picker refs at DROP time (the rows can leave the visible cache between
/// press and release; ids survive that, refs are cheap to build once).
/// Mutually exclusive with `DRAGGED` — a drag carries one payload kind, the
/// same invariant the picker's `Payload` enum enforces.
static DRAGGED_LOCAL_ROWS: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// The claimed drop target (sidebar playlist id) — mirrored Rust-side so
/// drag_end never reads the bridge off-thread.
static DRAG_OVER: Mutex<String> = Mutex::new(String::new());
/// The QUEUE drop target: the upcoming SLOT the row would land on, or -1 for
/// "not over the queue". Mirrored Rust-side for the same reason `DRAG_OVER`
/// is — `drag_end` runs off the Qt thread and must not read the bridge.
static DRAG_OVER_QUEUE: Mutex<i32> = Mutex::new(-1);

pub(crate) fn drag_start(
    track_id: String,
    title: String,
    subtitle: String,
    x: f32,
    y: f32,
    inline_visual: bool,
) {
    let id = track_id.parse::<u64>().unwrap_or(0);
    *DRAGGED.lock().unwrap() = if id > 0 { vec![id] } else { Vec::new() };
    DRAGGED_LOCAL_ROWS.lock().unwrap().clear();
    log::info!("[qbz-qt][drag] start {track_id} ({title})");
    shell_bridge::ui(move |mut b| {
        b.as_mut().set_drag_count(if id > 0 { 1 } else { 0 });
        b.as_mut().set_drag_title(QString::from(title.as_str()));
        b.as_mut()
            .set_drag_subtitle(QString::from(subtitle.as_str()));
        b.as_mut().set_drag_x(x);
        b.as_mut().set_drag_y(y);
        b.as_mut().set_drag_inline_visual(inline_visual);
        b.as_mut().set_drag_over_playlist_id(QString::default());
        b.as_mut().set_drag_over_queue_index(-1);
        b.as_mut().set_drag_active(true);
    });
}

/// LOCAL-mode twin of [`drag_start`]: a Local Library / Explorer row enters
/// the shared drag carrying its ROW ID, never a number that could be read as
/// a catalog id (the picker's `Payload` invariant, kept at the drag seam).
pub(crate) fn drag_start_local(
    row_id: String,
    title: String,
    subtitle: String,
    x: f32,
    y: f32,
    inline_visual: bool,
) {
    DRAGGED.lock().unwrap().clear();
    let ok = !row_id.is_empty();
    *DRAGGED_LOCAL_ROWS.lock().unwrap() = if ok { vec![row_id.clone()] } else { Vec::new() };
    log::info!("[qbz-qt][drag] start local row {row_id} ({title})");
    shell_bridge::ui(move |mut b| {
        b.as_mut().set_drag_count(if ok { 1 } else { 0 });
        b.as_mut().set_drag_title(QString::from(title.as_str()));
        b.as_mut()
            .set_drag_subtitle(QString::from(subtitle.as_str()));
        b.as_mut().set_drag_x(x);
        b.as_mut().set_drag_y(y);
        b.as_mut().set_drag_inline_visual(inline_visual);
        b.as_mut().set_drag_over_playlist_id(QString::default());
        b.as_mut().set_drag_over_queue_index(-1);
        b.as_mut().set_drag_active(true);
    });
}

pub(crate) fn drag_move(x: f32, y: f32) {
    shell_bridge::ui(move |mut b| {
        b.as_mut().set_drag_x(x);
        b.as_mut().set_drag_y(y);
    });
}

pub(crate) fn drag_set_over(playlist_id: String) {
    *DRAG_OVER.lock().unwrap() = playlist_id.clone();
    shell_bridge::ui(move |mut b| {
        b.as_mut()
            .set_drag_over_playlist_id(QString::from(playlist_id.as_str()));
    });
}

pub(crate) fn drag_set_over_queue(slot: i32) {
    *DRAG_OVER_QUEUE.lock().unwrap() = slot;
    shell_bridge::ui(move |mut b| {
        b.as_mut().set_drag_over_queue_index(slot);
    });
}

pub(crate) fn drag_end() {
    let pid = std::mem::take(&mut *DRAG_OVER.lock().unwrap());
    let queue_slot = std::mem::replace(&mut *DRAG_OVER_QUEUE.lock().unwrap(), -1);
    shell_bridge::ui(|mut b| {
        b.as_mut().set_drag_active(false);
        b.as_mut().set_drag_inline_visual(false);
        b.as_mut().set_drag_over_playlist_id(QString::default());
        b.as_mut().set_drag_over_queue_index(-1);
    });
    let tracks = std::mem::take(&mut *DRAGGED.lock().unwrap());
    let local_rows = std::mem::take(&mut *DRAGGED_LOCAL_ROWS.lock().unwrap());
    if tracks.is_empty() && local_rows.is_empty() {
        return;
    }

    // LOCAL-mode payload: Explorer / Local Library rows, resolved to
    // source-aware refs at drop time and routed through the picker's own
    // add matrix — the "same guardrails" contract: a library row id can
    // never reach a Qobuz endpoint, a Plex/remote row rides its server key.
    if !local_rows.is_empty() {
        let runtime = app();
        let pid = pid.clone();
        spawn(async move {
            let rows = tokio::task::spawn_blocking(move || {
                local_bulk::resolve_track_rows_blocking(&local_rows)
            })
            .await
            .unwrap_or_default();
            if rows.is_empty() {
                log::warn!("[qbz-qt][drag] local drop resolved no rows; ignored");
                return;
            }
            if queue_slot >= 0 {
                let was_empty = runtime
                    .core()
                    .get_queue_state_full()
                    .await
                    .upcoming
                    .is_empty()
                    && runtime.core().current_track().await.is_none();
                let mut landed = false;
                for (n, row) in rows.into_iter().enumerate() {
                    let qt = local_playback::local_queue_track(&row);
                    let slot = queue_slot as usize + n;
                    match queue_qt::insert_dragged_queue_track(&runtime, qt, slot).await {
                        Ok(()) => landed = true,
                        Err(e) => log::error!(
                            "[qbz-qt][drag] queue insert of local row {} at {slot} failed: {e}",
                            row.id
                        ),
                    }
                }
                if was_empty && landed {
                    queue_qt::set_drop_play_prompt(true);
                }
                return;
            }
            if pid.is_empty() {
                return;
            }
            let refs: Vec<String> = rows
                .iter()
                .map(local_playlist_qt::local_picker_ref_for_track)
                .collect();
            playlist_picker_qt::add_dropped_payload_to_target(
                &runtime,
                &pid,
                playlist_picker_qt::Payload::LocalRefs(refs),
            )
            .await;
        });
        return;
    }

    // The QUEUE target wins when both are claimed. They cannot both be under
    // the pointer geometrically, so this only decides a stale claim — and the
    // queue one is the more recent, since the panel overlays the sidebar.
    if queue_slot >= 0 {
        let runtime = app();
        let ids = tracks;
        spawn(async move {
            // Was the queue empty BEFORE any of this landed? Read it up front:
            // after the first insert it never is, and the answer decides
            // whether the panel asks about playback (owner ask 2026-08-10).
            let was_empty = runtime
                .core()
                .get_queue_state_full()
                .await
                .upcoming
                .is_empty()
                && runtime.core().current_track().await.is_none();
            let mut landed = false;
            for (n, id) in ids.into_iter().enumerate() {
                // Multi-drag lands in order: each successive row goes one slot
                // further down, so a 3-row drag keeps its own ordering instead
                // of arriving reversed.
                let slot = queue_slot as usize + n;
                match queue_qt::insert_dragged_track(&runtime, id, slot).await {
                    Ok(()) => landed = true,
                    Err(e) => {
                        log::error!("[qbz-qt][drag] queue insert of {id} at {slot} failed: {e}")
                    }
                }
            }
            // ONE prompt for the whole drop, and only when something actually
            // landed — a drop whose every row failed to resolve must not ask
            // about playing nothing.
            if was_empty && landed {
                queue_qt::set_drop_play_prompt(true);
            }
        });
        return;
    }
    let Ok(pid) = pid.parse::<u64>() else {
        // Not a Qobuz playlist target (a local-library playlist row or
        // nothing) — the sidecar path is out of scope (POC-NOTE).
        return;
    };
    let runtime = app();
    spawn(async move { playlist_qt::add_tracks(&runtime, pid, &tracks).await });
}

/// Card overlay Play for a playlist (fetch + play from the top).
pub(crate) fn play_playlist_by_id(playlist_id: u64) {
    let runtime = app();
    spawn(async move {
        if let Err(e) = playlist_qt::play_playlist_by_id(&runtime, playlist_id).await {
            log::error!("[qbz-qt] play playlist {playlist_id} failed: {e}");
        }
    });
}

/// Card menu queueing for a playlist.
pub(crate) fn enqueue_playlist_by_id(playlist_id: u64, mode: String) {
    let runtime = app();
    spawn(async move {
        if let Err(e) = playlist_qt::enqueue_playlist_by_id(&runtime, playlist_id, &mode).await {
            log::error!("[qbz-qt] enqueue playlist {playlist_id} ({mode}) failed: {e}");
        }
    });
}

/// Card overlay follow/unfollow for a playlist.
pub(crate) fn playlist_set_follow_by_id(playlist_id: u64, follow: bool) {
    let runtime = app();
    spawn(async move { playlist_qt::set_follow_by_id(&runtime, playlist_id, follow).await });
}

/// Now-Playing-view flyout: switch + persist the bar mode. Large forces
/// the sidebar open (the Slint "large" arm — the dock needs it).
pub(crate) fn npb_set_mode(mode: i32) {
    let mode = settings_qt::set_npb_mode(mode);
    log::info!("[qbz-qt] npb_mode -> {mode}");
    shell_bridge::ui(move |mut b| {
        if mode == 3 {
            b.as_mut().set_sidebar_state(0);
        }
        b.as_mut().set_npb_mode(mode);
    });
}

/// Large dock, cover eye button: show/hide the FFT band. Persists the pref,
/// republishes the dock height (the Sidebar's reservation and AppShell's pin
/// both read it), and gates the capture tap — the band being hidden is what
/// stops the FFT producer, so an unused visualizer costs zero CPU.
pub(crate) fn large_toggle_visualizer() {
    let on = !settings_qt::large_visualizer_on();
    settings_qt::set_large_visualizer_on(on);
    let height = shell_bridge::large_dock_height(on);
    log::info!("[qbz-qt] large visualizer -> {on} (dock height {height})");
    viz_qt::set_enabled(on);
    shell_bridge::ui(move |mut b| {
        b.as_mut().set_large_visualizer_on(on);
        b.as_mut().set_large_dock_height(height);
    });
}

/// Large dock, band click: cycle Bars -> Waveform -> Energy -> Goniometer ->
/// Oscilloscope on the GPU tier. Qt's software scene graph cannot draw the
/// two native scope traces, so that tier cycles only the three compatible
/// modes instead of landing on guide axes with no signal.
pub(crate) fn large_cycle_spectrum() {
    let gpu_tier = renderer_qt::gpu_tier();
    let current =
        settings_qt::large_spectrum_mode_for_tier(settings_qt::large_spectrum_mode(), gpu_tier);
    let mode_count = if gpu_tier { 5 } else { 3 };
    let mode = settings_qt::set_large_spectrum_mode((current + 1) % mode_count);
    let height = shell_bridge::large_dock_height(settings_qt::large_visualizer_on());
    log::info!("[qbz-qt] large spectrum mode -> {mode}");
    // Point the drain at the new stream BEFORE the UI switches, so the first
    // frame the new mode renders already has data.
    viz_qt::set_mode(mode);
    shell_bridge::ui(move |mut b| {
        b.as_mut().set_large_spectrum_mode(mode);
        b.as_mut().set_large_dock_height(height);
    });
}

/// Artist-network row click: resolve a musician NAME and open the right
/// surface for them.
///
/// This used to BE the whole feature, and it implemented exactly ONE of the
/// reference's five branches: a Confirmed match with a Qobuz id navigated, and
/// every other confidence logged a line and left the user staring at a row that
/// did nothing. Since the majority of credited session musicians resolve to
/// `contextual` or `weak`, the common case was a dead click.
///
/// The five-way dispatch now lives in `musician_qt` so that ALL SIX call sites
/// — the three artist-network groups, the album-credits modal, the desktop
/// track-info modal and the immersive track-info panel — behave identically and
/// no QML anywhere branches on confidence. See that module's header for the
/// branch table and for why `Some(0)` is not a valid id.
///
/// The function survives as a forwarder because `artist_bridge.rs:210` calls
/// `crate::resolve_musician`; replacing the body is a smaller blast radius than
/// re-pointing that bridge.
pub(crate) fn resolve_musician(name: String, role: String) {
    musician_qt::resolve_and_open(name, role);
}

/// Artist-card overlay play (ArtistGridCard): Popular tracks with the
/// studio-discography fallback.
pub(crate) fn play_artist_card(artist_id: String) {
    let runtime = app();
    spawn(async move {
        if let Err(e) = playback_qt::play_artist(&runtime, &artist_id).await {
            log::error!("[qbz-qt] play_artist_card {artist_id} failed: {e}");
        }
    });
}

/// Library track menu: Play next / Play later / Add to queue (single feed
/// track into the existing queue).
pub(crate) fn enqueue_track(track_id: u64, mode: String) {
    let runtime = app();
    spawn(async move {
        if let Err(e) = playback_qt::enqueue_single_track(&runtime, track_id, &mode).await {
            log::error!("[qbz-qt] enqueue_track {track_id} ({mode}) failed: {e}");
        }
    });
}

/// Album-card click on Home: resolve + enqueue + play through the core.
pub(crate) fn play_album(album_id: String) {
    let runtime = app();
    spawn(async move {
        if let Err(e) = playback_qt::play_album(&runtime, &album_id).await {
            log::error!("[qbz-qt] play_album failed: {e}");
        }
    });
}

pub(crate) fn transport_set_playing(playing: bool) {
    let runtime = app();
    spawn(async move { playback_qt::request_playing(&runtime, Some(playing)).await });
}

pub(crate) fn transport_toggle_play() {
    let runtime = app();
    spawn(async move { playback_qt::toggle_play(&runtime).await });
}

pub(crate) fn transport_next() {
    let runtime = app();
    spawn(async move { playback_qt::next(&runtime).await });
}

pub(crate) fn transport_previous() {
    let runtime = app();
    spawn(async move { playback_qt::previous(&runtime).await });
}

pub(crate) fn transport_seek(frac: f32) {
    let runtime = app();
    spawn(async move { playback_qt::seek_frac(&runtime, frac).await });
}

pub(crate) fn transport_seek_by(delta_secs: i32) {
    hotkeys_bridge::seek_relative(delta_secs);
}

pub(crate) fn ab_loop_mark() {
    let runtime = app();
    spawn(async move { ab_loop_qt::mark(runtime).await });
}

pub(crate) fn ab_loop_clear() {
    ab_loop_qt::clear();
}

pub(crate) fn transport_set_volume(volume: f32) {
    if now_playing::remote_volume_locked() {
        return;
    }
    // Local model first (instant UI), then the engine.
    now_playing::set_volume(volume);
    let runtime = app();
    spawn(async move { playback_qt::set_volume(&runtime, volume).await });
}

pub(crate) fn transport_toggle_mute() {
    if now_playing::remote_volume_locked() {
        return;
    }
    let runtime = app();
    spawn(async move { playback_qt::toggle_mute(&runtime).await });
}

pub(crate) fn transport_toggle_shuffle() {
    let runtime = app();
    spawn(async move { playback_qt::toggle_shuffle(&runtime).await });
}

pub(crate) fn transport_cycle_repeat() {
    let runtime = app();
    spawn(async move { playback_qt::cycle_repeat(&runtime).await });
}

// ============================ Library (phase 5) ===========================

/// Whether the Library view has been loaded this session — a STALENESS
/// latch, not a pure once-flag: favorite mutations reset it
/// (`mark_library_stale`), so the next Library visit refetches instead of
/// painting the session-start snapshot. #690: an album favorited mid-session
/// never appeared in the Library grid — the glyph caches updated, the FEED
/// membership only changed on restart (or a language switch, the one other
/// `reload_library` caller).
static LIBRARY_LOADED: Mutex<bool> = Mutex::new(false);

/// Favorite membership changed somewhere (album page heart, bulk bar, label
/// follow…): make the next Library visit reload the feed. Deliberately NOT a
/// live `reload_library()` — a per-click mutation must never republish a
/// document (the `toggle_pin` doctrine); the reload waits for the next
/// `hydrate_view("library")`.
pub(crate) fn mark_library_stale() {
    *LIBRARY_LOADED.lock().unwrap() = false;
}

/// TEMPORARY nav probe (QBZ_QT_NAV_PROBE=1) — the GUI-thread cost between the
/// click and the moment the route reaches QML. Remove with the fix.
pub(crate) fn nav_probe_on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var("QBZ_QT_NAV_PROBE").is_ok())
}
fn epoch_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// Sidebar navigation: record the view and lazy-load its data.
pub(crate) fn navigate_to(view: &str) {
    let probe = nav_probe_on().then(std::time::Instant::now);
    if probe.is_some() {
        log::info!("[navprobe] enter navigate_to({view}) at={}", epoch_ms());
    }
    nav_qt::record(view);
    if let Some(t) = probe {
        log::info!("[navprobe] record({view}) = {:?}", t.elapsed());
    }
    // A plain section navigation carries no landing tab: clear any request
    // a flyout click left behind, or the next mount of a tabbed view would
    // be dragged onto a stale tab.
    shell_bridge::ui(|mut b| {
        if !b.as_ref().nav_tab().is_empty() {
            b.as_mut().set_nav_tab(QString::default());
        }
    });
    LAST_DETAIL.lock().unwrap().0.clear();
    let th = nav_probe_on().then(std::time::Instant::now);
    hydrate_view(view);
    if let Some(t) = th {
        log::info!("[navprobe] hydrate({view}) = {:?}", t.elapsed());
    }
    if let Some(t) = probe {
        log::info!(
            "[navprobe] leave navigate_to({view}) sync={:?} at={}",
            t.elapsed(),
            epoch_ms()
        );
    }
}

/// The per-view lazy load that a section navigation owes its destination.
///
/// SPLIT OUT OF `navigate_to` BECAUSE `navigate_to` IS NOT THE ONLY DOOR.
/// Session restore ("Startup page = where you left off") mounts its view by
/// calling `nav_qt::record` DIRECTLY (`on_session_entered`), since it must not
/// clear the landing tab or the detail latch of a session that has not started
/// yet — and `record` is history bookkeeping, nothing more. So a restore onto
/// Library opened the Library view with `libraryJson` still `[]`: the page
/// rendered, the counts read zero, and it stayed that way until the user
/// clicked Library in the sidebar, which finally ran the arm below. Same hole
/// for Mixtapes and Collections. Home never showed it because `load_home_once`
/// is a separate, unconditional phase of shell entry.
///
/// `local` is absent ON PURPOSE and is not a gap: LocalLibraryView.qml loads
/// its own tab from `Component.onCompleted`, so the mount is the trigger there.
/// Detail routes are absent for the reason `navigate_to` already states — each
/// is reached through a call that loads its data and records the route itself.
fn hydrate_view(view: &str) {
    if view == "library" {
        load_library_once();
    }
    if view == "settings" {
        publish_settings();
    }
    // MyQBZ: BOTH grids reload on EVERY visit — deliberately no once-flag
    // (contrast `library` above). A create from the Add picker, a delete from
    // the detail view or a new Artist Collection from the builder all change
    // the set while the grid is unmounted, so a once-per-session flag would
    // paint a stale grid. The read is one local SQLite query.
    if view == "mixtapes" {
        myqbz_qt::load_grid(myqbz_qt::Grid::Mixtapes);
    }
    if view == "collections" {
        myqbz_qt::load_grid(myqbz_qt::Grid::Collections);
    }
    // "mixtapedetail", "discobuilder" and "blacklist" need no arm: each is
    // only ever reached through a call that loads its own data first and
    // records the route itself (myqbz_detail_qt::open, myqbz_builder_qt::open,
    // blacklist_qt::open_manager), never through the sidebar.
}

/// NavFlyout entry activation: navigate to `view` AND land on its internal
/// `tab` (Slint's `on_header_menu_navigate` per-route tab selection, e.g.
/// `discover-forYou` → `home::select_tab`, crates/qbz/src/main.rs:18026).
///
/// The tab travels on the bridge (navTab/navTabView/navTabSeq) and is
/// applied by a Binding in ContentRouter.qml — NOT by locating the view in
/// the scene: the pre-bridge port did that with a depth-capped tree walk
/// (NavFlyout's findTabHost), which the ContentRouter extraction (kiosk
/// port D3) silently outran, leaving every flyout entry navigating to the
/// section's default tab.
///
/// A click for the view already mounted skips `navigate_to`: it would
/// record a duplicate history entry, and the property bump below is enough
/// to re-apply the tab live.
pub(crate) fn navigate_to_tab(view: &str, tab: &str) {
    if nav_qt::current_view() != view {
        navigate_to(view);
    }
    let (view, tab) = (view.to_string(), tab.to_string());
    shell_bridge::ui(move |mut b| {
        let seq = b.as_ref().nav_tab_seq() + 1;
        b.as_mut().set_nav_tab_view(QString::from(view));
        b.as_mut().set_nav_tab(QString::from(tab));
        b.as_mut().set_nav_tab_seq(seq);
    });
}

// ============================ i18n (phase 20) ==============================

/// The active detail view ("album"/"artist" + id) — re-published on a live
/// language switch so its Rust-built section headers re-translate.
static LAST_DETAIL: Mutex<(String, String)> = Mutex::new((String::new(), String::new()));

/// The names the row that opened an album knew (id, title, artist): a 404
/// carries none, and the "no longer available" page needs them for its
/// heading and its alternatives search (album_qt::publish_unavailable).
static ALBUM_HINT: Mutex<Option<(String, String, String)>> = Mutex::new(None);

pub(crate) fn album_hint(album_id: &str) -> Option<(String, String)> {
    ALBUM_HINT
        .lock()
        .ok()
        .and_then(|guard| guard.clone())
        .filter(|(id, _, _)| id == album_id)
        .map(|(_, title, artist)| (title, artist))
}

/// `open_album` with the title/artist the caller knows — every row that
/// opens an album passes them, so the page can name a release the catalog
/// no longer has.
pub(crate) fn open_album_from(album_id: String, title: String, artist: String) {
    if let Ok(mut guard) = ALBUM_HINT.lock() {
        *guard = Some((album_id.clone(), title, artist));
    }
    open_album(album_id);
}

/// Settings > Appearance > Language: the pref is already persisted by the
/// settings arm; this applies it LIVE — "auto" resolves POSIX env — then:
///  1. bumps trRev, so EVERY `QbzBridge.tr(msgid, QbzBridge.trRev)` binding
///     re-evaluates through the new catalog (the Slint reseed equivalent);
///  2. re-publishes the Rust-built translated documents (home section
///     titles, library feed labels, settings option labels, the active
///     album/artist detail headers — tf() plurals included).
pub(crate) fn apply_language(code: String) {
    let resolved = if code == "auto" {
        qbz_i18n::resolve_auto().to_string()
    } else {
        code.clone()
    };
    qbz_i18n::set_language(&resolved);
    log::info!("[qbz-qt] language -> {code} (resolved: {resolved})");
    session_bridge::ui(move |mut b| {
        let next = b.as_ref().tr_rev() + 1;
        b.as_mut().set_tr_rev(next);
    });
    // §3.4/F12 (2026-08-03 hotkeys-port): the hotkeys groups document's
    // action labels are Rust-translated (`qbz_i18n::t`), so they cannot
    // re-translate through trRev — recompute on the same bump.
    hotkeys_bridge::ui(|h| h.refresh());
    reload_home();
    reload_library();
    publish_settings();
    // MyQBZ builds a handful of strings in RUST — the kind eyebrow
    // ("MIXTAPE" / "COLLECTION" / "ARTIST"), the "{} albums" plural, every
    // unresolved row's TYPE label, and the builder's footer count + default
    // collection name. None of them re-translate through trRev, because they
    // arrive as data inside a JSON document, so each owner republishes.
    //
    // Unconditional, unlike the album/artist republish below: `navigate_to`
    // clears LAST_DETAIL, so routing MyQBZ through that latch would silently
    // skip it. Each of these is a no-op on an empty document.
    myqbz_qt::republish_all();
    myqbz_detail_qt::republish();
    myqbz_builder_qt::republish();
    // Same reason, same unconditional call: the discography page's header
    // title is `artist_qt::release_type_title(...)`, a Rust-translated string
    // living inside a JSON document, and LAST_DETAIL only ever latches "album"
    // or "artist" — it cannot reach this page. No-op when nothing is open.
    artist_releases_qt::republish();
    // Same class again: the musician page publishes a Rust-translated error
    // line INSIDE its JSON document (`musician_qt.rs:679,781`), and `trRev`
    // cannot reach a string that travels as data. LAST_DETAIL only ever
    // latches "album" or "artist", so this page is unreachable through the
    // match below. No-op when nothing is open.
    //
    // The Artist Scene deliberately has NO counterpart here: it translates
    // nothing in Rust — every string on that view is a QML
    // `QbzSession.tr(..., trRev)`, including the discovery phase labels and the
    // card's (always empty) subtitle — so it re-translates by itself and a
    // republish would be dead code.
    musician_qt::republish();
    // Purchases, for the same reason: both of its documents carry exactly one
    // Rust-translated string (the load-error line), and a string travelling as
    // data cannot be reached by `trRev`. A no-op unless a purchase screen is
    // standing in its error state — which is also the only state the owner's
    // own account can produce, so it is the one worth getting right.
    purchases_qt::republish();
    // Browse and Label also embed translated strings inside their JSON
    // documents (playlist-count plurals, jump tabs and the Unknown bucket).
    // `trRev` cannot reach those values, so refresh their live snapshots too.
    browse_qt::republish_for_language();
    label_qt::republish_for_language();
    let (view, id) = LAST_DETAIL.lock().unwrap().clone();
    if !id.is_empty() {
        match view.as_str() {
            "album" => republish_album(id),
            "artist" => republish_artist(id),
            _ => {}
        }
    }
}

/// Re-fetch + publish the album/artist detail doc WITHOUT touching nav
/// (the open_* entry points record a nav entry).
fn republish_album(album_id: String) {
    if offline_fwd::engine().status().is_offline() {
        return;
    }
    let runtime = app();
    spawn(async move {
        if let Ok(json) = album_qt::load_album_view(&runtime, &album_id).await {
            album_bridge::ui(move |mut b| b.as_mut().set_album_json(QString::from(json.as_str())));
        }
    });
}

/// Republish whichever detail page is open, if it is the artist page.
///
/// Exists for the MusicBrainz toggle. Every MB-derived field on the artist
/// document — `mbAvailable`, the whole Origin block, the relationship groups,
/// and `origin.locationClickable`, which is the gate on BOTH Artist Scene
/// doors — is baked at document-build time. Flipping the setting updates only
/// the core client, so without this a page the user navigates back to keeps
/// offering an affordance the client can no longer serve, and the discovery
/// call would run against a disabled MusicBrainz and report a false "no
/// artists found" (the shared core swallows the disabled-client error).
///
/// No-op when the open detail is not an artist, and `republish_artist` is
/// itself a no-op offline.
pub(crate) fn republish_open_artist() {
    let (view, id) = LAST_DETAIL.lock().unwrap().clone();
    if view == "artist" && !id.is_empty() {
        republish_artist(id);
    }
}

fn republish_artist(artist_id: String) {
    if offline_fwd::engine().status().is_offline() {
        return;
    }
    let runtime = app();
    spawn(async move {
        if let Ok(json) = artist_qt::load_artist_view(&runtime, &artist_id).await {
            artist_bridge::ui(move |mut b| {
                b.as_mut().set_artist_json(QString::from(json.as_str()))
            });
        }
    });
}

// ============================ Search (phase 15) ===========================

pub(crate) fn search_live(query: String) {
    // The live query is published FIRST, before the offline bail and before
    // anything async: it is what Enter and "View more" submit, and it must
    // describe what the user has typed, never what last finished loading.
    search_bridge::set_cortinilla_query(query.trim().to_string());
    // NO offline early return. The cortinilla has on-device sections now, so
    // offline is precisely when it earns its keep: the Qobuz half fails, the
    // local half answers, and `live()` degrades to a local-only payload with
    // WIDENED caps. Returning here made the dropdown dead exactly where the
    // user has nothing else.
    let runtime = app();
    spawn(async move { search_qt::live(&runtime, &query).await });
}

pub(crate) fn search_submit(query: String) {
    // No offline early return either: `submit` publishes the results page,
    // and its Err arm already preserves the query and renders the page's own
    // empty state. Returning left the PREVIOUS page on screen, so pressing
    // Enter offline looked like the app had ignored the keystroke.
    let runtime = app();
    spawn(async move { search_qt::submit(&runtime, &query, None).await });
}

pub(crate) fn cortinilla_view_more(kind: String) {
    let runtime = app();
    spawn(async move { search_qt::view_more(&runtime, &kind).await });
}

pub(crate) fn cortinilla_search_all() {
    let runtime = app();
    spawn(async move { search_qt::search_all_action(&runtime).await });
}

pub(crate) fn search_load_more(tab: i32) {
    let runtime = app();
    spawn(async move { search_qt::load_more(&runtime, tab).await });
}

pub(crate) fn search_filter_changed(index: i32) {
    let runtime = app();
    spawn(async move { search_qt::filter_changed(&runtime, index).await });
}

// ============================ Settings (phase 10) =========================

/// Publish the settings snapshot (settings_qt.rs SettingsDoc) onto
/// `settingsJson`. Called on settings-view open and after every mutation
/// (the handlers republish themselves).
pub(crate) fn publish_settings() {
    spawn(async { settings_qt::publish_snapshot().await });
}

pub(crate) fn settings_bool(key: String, value: bool) {
    let runtime = app();
    spawn(async move { settings_qt::settings_bool(&runtime, &key, value).await });
}

pub(crate) fn settings_select(key: String, index: i32) {
    let runtime = app();
    spawn(async move { settings_qt::settings_select(&runtime, &key, index.max(0) as usize).await });
}

pub(crate) fn settings_slider(key: String, value: i32) {
    let runtime = app();
    spawn(async move { settings_qt::settings_slider(&runtime, &key, value).await });
}

pub(crate) fn settings_string(key: String, value: String) {
    spawn(async move { settings_qt::settings_string(&key, value).await });
}

pub(crate) fn settings_reset() {
    let runtime = app();
    spawn(async move { settings_qt::settings_reset(&runtime).await });
}

pub(crate) fn refresh_devices() {
    let runtime = app();
    spawn(async move { settings_qt::refresh_devices(&runtime).await });
}

/// Appearance > Theme row: persist the slug + republish the token document
/// (live switch — QbzTheme.qml rebinds every consumer).
pub(crate) fn theme_set(slug: String) {
    // FIRST-EVER selection of "Custom": seed the editable base from the
    // palette the user is looking at RIGHT NOW, so picking Custom customizes
    // what they see instead of jumping to OLED black (1:1 with the reference,
    // crates/qbz/src/main.rs:11414-11422).
    //
    // The snapshot is taken BEFORE `set_theme`, and that ordering is the whole
    // fix: `set_theme` persists the slug and `current_slug()` re-reads it from
    // disk, so a seed taken afterwards would resolve "custom", find no file,
    // and snapshot the default it was supposed to replace.
    // Entering Custom re-reads the file. The in-memory base is memoized for
    // the life of the process (so a drag does not re-read it), and it is
    // filled at bridge construction for every user — including one who booted
    // with no file and whose theme the SHIPPING SLINT BUILD wrote afterwards.
    // Without this the cached OLED default would win over their actual theme.
    if slug == "custom" {
        custom_theme_qt::invalidate();
    }
    let seed_from = (slug == "custom" && !custom_theme_qt::exists())
        .then(|| theme_qt::colors_for_slug(&theme_qt::current_slug()));
    theme_qt::set_theme(&slug);
    if let Some(prev) = seed_from {
        custom_theme_qt::seed_from_current(&prev);
    }
    theme_qt::publish_theme();
    // The editor's swatches ride their own document: republish so the grid is
    // correct the instant the rows appear (and after a seed rewrote the base).
    // Only under "custom" — anywhere else the editor is not mounted, and the
    // call would lazily read custom_theme.json for nothing.
    if slug == "custom" {
        custom_theme_qt::publish_state();
    }
}

/// Appearance > theme filter cycle (0 All / 1 Dark / 2 Light).
pub(crate) fn theme_set_filter(index: i32) {
    let index = index.clamp(0, 2);
    theme_qt::set_theme_filter(index);
    shell_bridge::ui(move |mut b| b.as_mut().set_theme_filter(index));
}

/// Integrations panel non-toggle actions (Last.fm connect flow, LB/LFM
/// disconnects).
pub(crate) fn integrations_action(action: String) {
    let runtime = app();
    spawn(async move { integrations_qt::handle_action(&runtime, &action).await });
}

/// App-menu chrome toggle: persist the flipped pref and update the menu
/// state. The APPLIED mode (`systemTitleBar`) is deliberately untouched —
/// it changes on the next launch (the window flags are read once at
/// startup, 1:1 the Slint restart semantics).
pub(crate) fn toggle_system_title_bar() {
    let next = settings_qt::toggle_system_title_bar();
    log::info!("[qbz-qt] use_system_title_bar -> {next} (applies on next launch)");
    shell_bridge::ui(move |mut b| b.as_mut().set_system_title_bar_pref(next));
}

/// App-menu ambient toggle: persist the flipped pref and apply LIVE (the
/// ambient layer is pure QML — no restart needed).
pub(crate) fn toggle_ambient_background() {
    let mode = settings_qt::toggle_ambient_background();
    log::info!(
        "[qbz-qt] app_background -> {}",
        match mode {
            1 => "ambient",
            2 => "blurred",
            _ => "off",
        }
    );
    shell_bridge::ui(move |mut b| b.as_mut().set_ambient_mode(mode));
}

/// ArtistView's "In library" toggle: ask for the feed (a fetch only if it has
/// never loaded this session — `load_library_once`), and repaint the open
/// page's rows from the cache when it is already warm. The owner's rule for
/// the toggle: re-arm the warm, but SERVE FROM CACHE — clicking the tab must
/// never refetch a 10k-row feed.
pub(crate) fn warm_library() {
    load_library_once();
    artist_qt::refresh_library_items();
}

fn load_library_once() {
    if *LIBRARY_LOADED.lock().unwrap() {
        return;
    }
    if offline_fwd::engine().status().is_offline() {
        log::info!("[qbz-qt] library load skipped (offline session)");
        return;
    }
    *LIBRARY_LOADED.lock().unwrap() = true;
    reload_library();
}

/// Fetch + publish the whole library (feed + counts). Perf-instrumented
/// (phase-5 deliverable): wall timings land in the log.
pub(crate) fn reload_library() {
    if offline_fwd::engine().status().is_offline() {
        return;
    }
    log_rss("library load start");
    library_bridge::ui(|mut b| {
        b.as_mut().set_library_loading(true);
        b.as_mut().set_library_error(QString::from(""));
    });
    let runtime = app();
    spawn(async move {
        let t = std::time::Instant::now();
        match library_qt::load_library(&runtime).await {
            Ok(total) => {
                let deletion_revision = library_qt::playlist_deletion_revision();
                let t_ser = std::time::Instant::now();
                let (feed_json, counts_json) = library_qt::with_library(|d| {
                    (
                        serde_json::to_string(&d.feed).unwrap_or_else(|_| "[]".into()),
                        serde_json::to_string(&d.counts).unwrap_or_else(|_| "{}".into()),
                    )
                })
                .unwrap_or_else(|| ("[]".into(), "{}".into()));
                log::info!(
                    "[qbz-qt][perf] library serialize: {:?} ({} bytes)",
                    t_ser.elapsed(),
                    feed_json.len(),
                );
                log::info!(
                    "[qbz-qt][perf] library load total: {:?} ({total} items)",
                    t.elapsed(),
                );
                library_bridge::ui(move |mut b| {
                    if deletion_revision != library_qt::playlist_deletion_revision() {
                        b.as_mut().set_library_loading(false);
                        return;
                    }
                    b.as_mut()
                        .set_library_json(QString::from(feed_json.as_str()));
                    b.as_mut()
                        .set_library_counts_json(QString::from(counts_json.as_str()));
                    b.as_mut().set_library_loading(false);
                });
                log_rss("library published");
                // The artist page open right now (if any) was built while the
                // feed was cold — hand it its "In library" rows.
                artist_qt::refresh_library_items();
            }
            Err(e) => {
                log::warn!("[qbz-qt] library load failed: {e}");
                library_bridge::ui(move |mut b| {
                    b.as_mut().set_library_error(QString::from(e.as_str()));
                    b.as_mut().set_library_loading(false);
                });
            }
        }
    });
}

/// Re-publish the Library document from the CACHED feed — no fetch, no db read.
/// The `publish_sidebar()` of this domain.
///
/// Reserved for the mutations that REMOVE or ADD a row, which is the one thing
/// the in-place `libraryFavoriteChanged` / `pinChanged` signals cannot express:
/// they patch a badge on a row that stays, while an unfollow has to make the
/// row leave. Everything else must keep using the signals — see the long note
/// on `library_toggle_favorite` about `QQuickItemView::setModel()`.
///
/// Handing `model:` a new array is exactly what a tab switch already does, and
/// the crash that note describes was reading `.model` BACK off the view inside
/// the teardown; `LibraryView.visibleRows` no longer does that. What the user
/// does pay is the scroll offset, which is why this is not a per-click verb.
///
/// No-op before the Library has ever loaded (`with_library` -> `None`).
pub(crate) fn publish_library_document() {
    let deletion_revision = library_qt::playlist_deletion_revision();
    let Some((feed_json, counts_json)) = library_qt::with_library(|d| {
        (
            serde_json::to_string(&d.feed).unwrap_or_else(|_| "[]".into()),
            serde_json::to_string(&d.counts).unwrap_or_else(|_| "{}".into()),
        )
    }) else {
        return;
    };
    library_bridge::ui(move |mut b| {
        if deletion_revision != library_qt::playlist_deletion_revision() {
            return;
        }
        b.as_mut()
            .set_library_json(QString::from(feed_json.as_str()));
        b.as_mut()
            .set_library_counts_json(QString::from(counts_json.as_str()));
    });
}

/// VmRSS (KiB) from /proc/self/status — phase-5 RSS-delta measurement.
fn log_rss(mark: &str) {
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        if let Some(line) = status.lines().find(|l| l.starts_with("VmRSS:")) {
            log::info!("[qbz-qt][perf] RSS @ {mark}: {line}");
        }
    }
}

/// Windowed artwork dispatch for the Library feed: the view reports the
/// mounted window as artKeys; download the missing ones, emitting
/// `libraryArtworkReady` per store (id-keyed — the wrong-cover race fix
/// from the Slint round). Disk hits emit immediately.
pub(crate) fn library_artwork_window(keys_json: String) {
    library_artwork_window_at_px(keys_json, None);
}

pub(crate) fn library_artwork_window_at_px(keys_json: String, pixels: Option<i32>) {
    let keys: Vec<String> = serde_json::from_str(&keys_json).unwrap_or_default();
    log::debug!("[qbz-qt] artwork window: {} keys", keys.len());
    if keys.is_empty() {
        return;
    }
    let Some(pairs) = library_qt::with_library(|d| {
        keys.iter()
            .filter_map(|k| {
                d.feed
                    .iter()
                    .find(|i| &i.art_key == k)
                    .map(|i| (i.art_key.clone(), i.image_url.clone()))
            })
            .filter(|(_, u)| !u.is_empty())
            .collect::<Vec<_>>()
    }) else {
        return;
    };
    let mut missing: Vec<(String, String)> = Vec::new();
    for (key, url) in pairs {
        let url = pixels.map_or_else(|| url.clone(), |px| myqbz_qt::kiosk_art_url(&url, px));
        let path = artwork_qt::cached_path(&url);
        if path.is_empty() {
            missing.push((key, url));
        } else {
            emit_library_artwork(key, path);
        }
    }
    if missing.is_empty() {
        return;
    }
    spawn(async move {
        let urls = missing.iter().map(|(_, url)| url.clone()).collect();
        let mut keys_by_url = std::collections::HashMap::<String, Vec<String>>::new();
        for (key, url) in missing {
            keys_by_url.entry(url).or_default().push(key);
        }
        artwork_qt::download_missing_progressive(urls, move |url, path| {
            if let Some(keys) = keys_by_url.get(&url) {
                for key in keys {
                    emit_library_artwork(key.clone(), path.clone());
                }
            }
        })
        .await;
    });
}

fn emit_library_artwork(key: String, path: String) {
    library_bridge::ui(move |mut b| {
        b.as_mut()
            .library_artwork_ready(QString::from(key.as_str()), QString::from(path.as_str()));
    });
}

/// Card heart: toggle + signal the result (or the unchanged state on
/// failure, so the UI rolls back).
///
/// The rollback half only became real once `library_qt::toggle_favorite`
/// started returning `Some(current)` on a failed write instead of `None` —
/// while it returned None this emitted nothing and every optimistic flip
/// stuck, which is how a 404'd un-favorite still read as un-favorited until
/// the next library reload. NOTE the signal is only USEFUL where somebody
/// connects it. Listeners today: `LibraryView.qml`, `AlbumView.qml` (header
/// heart, `album:{id}`) and `ArtistView.qml` (header follow `artist:{id}` +
/// its Popular Tracks / Appears On rows, `track:{id}`), plus — since the round
/// that made the optimistic flips reconcilable — `cards/AlbumCard`,
/// `cards/ArtistCard`, `cards/PlaylistCard`, `rows/TrackRow`, `TrackCard`,
/// `QueuePanel` and `shell/PlayerBar`. So every heart in the app settles here.
///
/// Do NOT "fix" a stale heart by republishing a document from a toggle. That
/// hands `model:` a new array, and `QQuickItemView::setModel()` resets the
/// scroll offset AND tears down the QQmlDelegateModel — which is this build's
/// only crash signature (a null read at libQt6QmlModels[420c5], reproduced
/// live). The signal exists so a badge can be patched in place instead.
pub(crate) fn library_toggle_favorite(kind: String, id: String) {
    let runtime = app();
    spawn(async move {
        if let Some(value) = library_qt::toggle_favorite(&runtime, &kind, &id).await {
            settle_favorite(&runtime, &kind, &id, value).await;
        }
    });
}

/// One release-wide removal at a time per album id. The confirmation modal
/// closes before the async writes settle, so without a backend latch the same
/// menu action could issue duplicate deletes while the first batch is live.
static RELEASE_FAVORITE_REMOVALS: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

pub(crate) fn library_remove_release_favorites(album_id: String) {
    if album_id.is_empty() {
        return;
    }
    {
        let mut in_flight = RELEASE_FAVORITE_REMOVALS
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !in_flight.insert(album_id.clone()) {
            return;
        }
    }

    let runtime = app();
    spawn(async move {
        let targets = library_qt::release_favorite_targets(&album_id);
        let mut removed: Vec<(String, String)> = Vec::new();
        let mut failed = 0usize;

        for (kind, id) in targets {
            match runtime.core().remove_favorite(&kind, &id).await {
                Ok(()) => {
                    library_qt::set_feed_favorite(&kind, &id, false);
                    fav_cache_qt::set(&kind, &id, false);
                    removed.push((kind, id));
                }
                Err(error) => {
                    failed += 1;
                    log::error!(
                        "[qbz-qt] remove release favourites {album_id}: {kind}:{id} failed: {error}"
                    );
                }
            }
        }

        // Reconcile every confirmed server write before publishing, then
        // replace the Library model at most once. A purchase representative
        // stays and merely loses its heart.
        let mut membership_changed = false;
        for (kind, id) in &removed {
            membership_changed |=
                library_qt::reconcile_qobuz_favorite(&runtime, kind, id, false).await;
            emit_library_favorite(kind, id, false);
        }
        if membership_changed {
            publish_library_document();
        }

        RELEASE_FAVORITE_REMOVALS
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(&album_id);

        if removed.is_empty() && failed == 0 {
            toast_qt::info(qbz_i18n::t("No favorites from this release to remove"));
        } else if failed == 0 {
            toast_qt::success(qbz_i18n::t("Favorites removed from this release"));
        } else if removed.is_empty() {
            toast_qt::error(qbz_i18n::t("Could not remove favorites from this release"));
        } else {
            toast_qt::error(qbz_i18n::t("Some favorites could not be removed"));
        }
    });
}

/// A heart's write LANDED with `value`: reconcile the cached feed (a fresh
/// Qobuz favourite joins it, a cleared one leaves; local hearts have their
/// own row pair), fan the settled value out through `emit_library_favorite`,
/// and republish the Library document only when a row came or went.
///
/// Reconcile BEFORE emit, deliberately: the fan-out reaches
/// `artist_qt::apply_favorite_change`, which recomputes the open artist's
/// "In library" rows off the feed — it has to see the inserted / removed row
/// or the tab keeps its stale count (the #737 round's "the badge never
/// moves"). Every settle point that owns a Qobuz heart goes through here
/// (`library_toggle_favorite`, the queue panel heart in `queue_qt`).
pub(crate) async fn settle_favorite(
    runtime: &Arc<AppRuntime<LoggingAdapter>>,
    kind: &str,
    id: &str,
    value: bool,
) {
    let mut membership_changed = !value && library_qt::remove_local_favorite_row(kind, id);
    membership_changed |= library_qt::reconcile_qobuz_favorite(runtime, kind, id, value).await;
    emit_library_favorite(kind, id, value);
    if membership_changed {
        publish_library_document();
    }
}

/// THE settle point for a heart, wherever the click came from: emit the
/// in-place signal, then fan the settled value into the document caches.
///
/// Split out of `library_toggle_favorite` so a domain that owns its own header
/// state (playlist_qt) can settle BOTH its document and the feed row from the
/// one authoritative answer instead of guessing twice — and now so that every
/// mutation site reaches the fan-out through one door.
///
/// The fan-out is the favourite twin of `toggle_pin`'s, and it exists because
/// build-time stamping alone is not enough: the three caches below are
/// SNAPSHOTS that get re-serialized later (Discover's section configurator,
/// search's post-`submit` artwork pass, a reco tab re-entry), and a snapshot
/// re-published after a toggle put the pre-toggle heart back on screen — over
/// an item whose real state had changed, so the next click did the opposite of
/// what the glyph advertised. Each patches its own rows and publishes NOTHING;
/// the cards on screen are already correct from their optimistic flip and the
/// `libraryFavoriteChanged` signal.
///
/// NOT yet covered — the PAGE-lifetime documents, whose republish window is
/// the couple of seconds between a page opening and its artwork/deferred rows
/// landing: `album_qt` (header + track rows), `artist_qt` (header + top
/// tracks), `playlist_qt` (track rows), `label_qt` (header + cards + top
/// tracks), `browse_qt` (cards). They are listed here rather than left
/// unmentioned; the fix is one `apply_favorite_change` per module, identical
/// in shape to the three below.
pub(crate) fn emit_library_favorite(kind: &str, id: &str, value: bool) {
    let key = library_qt::feed_key(kind, id);
    library_bridge::ui(move |mut b| {
        b.as_mut()
            .library_favorite_changed(QString::from(key.as_str()), value);
    });
    home_qt::apply_favorite_change(kind, id, value);
    recommendations_qt::apply_favorite_change(kind, id, value);
    search_qt::apply_favorite_change(kind, id, value);
    artist_qt::apply_favorite_change(kind, id, value);
}

/// Dev navigation driver — `QBZ_QT_NAV_BENCH=home,library,home,settings`.
///
/// A hop may name a landing tab as `view:tab` (`library:all`,
/// `local:tracks`). That matters for measuring: a tab switch inside an
/// already-mounted view changes no route, so the route stopwatch never fires
/// for it — hopping AWAY and back with a landing tab is what puts a given
/// tab's real cost on the clock.
///
/// Two jobs, both of which the project had to do by hand before:
///
///  1. **Measuring a route change.** The cost of a mount is wall-clock on the
///     UI thread, so it cannot be read off a `cargo test` or a static audit;
///     it needs the real app to actually navigate. Paired with
///     `QT_LOGGING_RULES="qbz.nav.timing.info=true"` (the probes in
///     ContentRouter.qml / HomeView.qml) this prints what each hop cost.
///  2. **Mounting views the offscreen gate never reaches.** That gate keys off
///     `QbzShell.currentView` and nobody navigates during it, so it only ever
///     instantiates the startup view — which is how two entirely unloadable
///     modals and a live `ReferenceError` survived fifteen parity rounds. A
///     hop list makes those load errors fire where the gate can see them.
///
/// Runs only when the variable is set, off its own thread, and starts after
/// the first home publish so the hops land on a populated app rather than on
/// empty documents. `QBZ_QT_NAV_BENCH_MS` (default 4000) is the dwell per hop:
/// long enough for the mount, its artwork republish and the settle to finish
/// before the next hop muddies the log.
fn nav_bench_if_requested() {
    let Ok(list) = std::env::var("QBZ_QT_NAV_BENCH") else {
        return;
    };
    let hops: Vec<String> = list
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    if hops.is_empty() {
        return;
    }
    let dwell = std::env::var("QBZ_QT_NAV_BENCH_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(4000);
    std::thread::Builder::new()
        .name("qbz-qt-nav-bench".to_string())
        .spawn(move || {
            for hop in hops {
                std::thread::sleep(std::time::Duration::from_millis(dwell));
                log::info!("[qbz-qt][navbench] -> {hop}");
                // `navigate_to` / `navigate_to_tab` are the SAME entry points a
                // sidebar click and a nav-flyout entry use (they record history
                // and republish `currentView`), so a hop exercises the real
                // route path, per-view load arms included — not a shortcut that
                // writes the property directly.
                match hop.split_once(':') {
                    Some((view, tab)) => navigate_to_tab(view, tab),
                    None => navigate_to(&hop),
                }
            }
            log::info!("[qbz-qt][navbench] done");
        })
        .ok();
}

/// `reloadHome()` invokable / auto-load worker: fetch + publish + artwork.
pub(crate) fn reload_home() {
    if offline_fwd::engine().status().is_offline() {
        return;
    }
    home_bridge::ui(|mut b| {
        b.as_mut().set_home_loading(true);
        b.as_mut().set_home_error(QString::from(""));
    });
    let runtime = app();
    spawn(async move {
        match home_qt::load_home(&runtime).await {
            Ok(mut sections) => {
                // Artwork: disk hits attach synchronously; misses download
                // in the background and trigger ONE republish (POC-NOTE in
                // artwork_qt.rs — per-row model updates are the follow-up).
                let mut missing = artwork_qt::attach_cached(&mut sections.home);
                missing.extend(artwork_qt::attach_cached(&mut sections.editor));
                missing.extend(artwork_qt::attach_cached(&mut sections.for_you));
                missing.dedup();
                let count: usize = sections.home.iter().map(|s| s.items.len()).sum::<usize>()
                    + sections.editor.iter().map(|s| s.items.len()).sum::<usize>()
                    + sections
                        .for_you
                        .iter()
                        .map(|s| s.items.len())
                        .sum::<usize>();

                publish_home_sections(&sections);
                // LIVENESS. The shell has a mounted view with real content, so
                // whatever was restored this boot did not kill it — clear the
                // crash chain for the next start.
                nav_qt::mark_startup_healthy();
                nav_bench_if_requested();
                log::info!(
                    "[qbz-qt] home published: {}+{}+{} sections, {} cards, {} artwork misses",
                    sections.home.len(),
                    sections.editor.len(),
                    sections.for_you.len(),
                    count,
                    missing.len(),
                );
                if !kiosk_profile_qt::active() && !missing.is_empty() {
                    spawn(async move {
                        artwork_qt::download_missing(missing).await;
                        let mut sections = sections;
                        let _ = artwork_qt::attach_cached(&mut sections.home);
                        let _ = artwork_qt::attach_cached(&mut sections.editor);
                        let _ = artwork_qt::attach_cached(&mut sections.for_you);
                        publish_home_sections(&sections);
                        log::info!("[qbz-qt] home republished after artwork downloads");
                    });
                }

                home_bridge::ui(|mut b| b.as_mut().set_home_loading(false));
            }
            Err(e) => {
                log::warn!("[qbz-qt] home load failed: {e}");
                home_bridge::ui(move |mut b| {
                    b.as_mut().set_home_error(QString::from(e.as_str()));
                    b.as_mut().set_home_loading(false);
                });
            }
        }
    });
}

fn publish_home_sections(sections: &home_qt::DiscoverSections) {
    let home_json = serde_json::to_string(&sections.home).unwrap_or_else(|_| "[]".to_string());
    let editor_json = serde_json::to_string(&sections.editor).unwrap_or_else(|_| "[]".to_string());
    let for_you_json =
        serde_json::to_string(&sections.for_you).unwrap_or_else(|_| "[]".to_string());
    home_bridge::ui(move |mut b| {
        b.as_mut()
            .set_home_sections_json(QString::from(home_json.as_str()));
        b.as_mut()
            .set_editor_sections_json(QString::from(editor_json.as_str()));
        b.as_mut()
            .set_for_you_sections_json(QString::from(for_you_json.as_str()));
    });
}

/// Does an explicit Qt backend environment variable actually override the
/// renderer preference?
///
/// `var_os(..).is_some()` answers YES for an EMPTY value, and an empty value
/// is exactly what a shell or a test harness leaves behind when it clears a
/// variable rather than unsetting it: `QSG_RHI_BACKEND= ./qbz` on Linux, and
/// on Windows either `$env:QSG_RHI_BACKEND = ''` or
/// `[Environment]::SetEnvironmentVariable('QSG_RHI_BACKEND', $null, 'Process')`
/// — PowerShell coerces `$null` to `String.Empty` for a `string` parameter, so
/// that call leaves the name PRESENT and empty, and a child process inherits
/// it that way. (`$env:QSG_RHI_BACKEND = $null` is the one that removes it.)
/// The whole preference then silently stopped applying, the persisted Settings
/// choice included, and the only clue was one info line claiming an explicit
/// backend was present.
///
/// `QBZ_RENDERER`, read a few lines below, has always filtered the empty
/// string. This is the same rule for the two Qt variables.
fn qt_backend_env_overrides(name: &str) -> bool {
    env_value_overrides(std::env::var_os(name))
}

/// The predicate above, split from the lookup so it can be tested without
/// mutating process-global state, which races other tests in the same binary.
fn env_value_overrides(value: Option<std::ffi::OsString>) -> bool {
    value
        .map(|v| !v.to_string_lossy().trim().is_empty())
        .unwrap_or(false)
}

#[cfg(test)]
mod renderer_env_tests {
    use super::env_value_overrides;
    use std::ffi::OsString;

    #[test]
    fn an_absent_variable_does_not_override() {
        assert!(!env_value_overrides(None));
    }

    #[test]
    fn an_empty_variable_does_not_override() {
        // The defect: `QSG_RHI_BACKEND=` disabled the entire renderer
        // preference, the user's persisted setting included.
        assert!(!env_value_overrides(Some(OsString::from(""))));
        assert!(!env_value_overrides(Some(OsString::from("   "))));
    }

    #[test]
    fn a_real_value_still_overrides() {
        assert!(env_value_overrides(Some(OsString::from("opengl"))));
        assert!(env_value_overrides(Some(OsString::from(" d3d11 "))));
    }
}

/// The Settings > Appearance RENDERER row, consumed at startup (PARITY-DEBT
/// #104 — the row rendered and persisted into the SAME ui_prefs the Slint
/// reads, but nothing consumed it, so it lied).
///
/// Precedence (Slint `requested_renderer_tier`, main.rs:7825-7848): explicit
/// Qt envs (`QSG_RHI_BACKEND` / `QT_QUICK_BACKEND`) > `QBZ_RENDERER` > the
/// persisted pref > Qt's default backend (Metal on macOS, OpenGL on Linux —
/// both measured healthy, 2026-08-01/02). Cross-frontend mapping: the GPU
/// tiers (`auto`/`wgpu`/`gpu`/`hardware`/`hw`) ARE Qt's default on both
/// platforms; `gl` maps to `QSG_RHI_BACKEND=opengl` on Linux and to the
/// Metal default on macOS (the Slint remaps GL to skia(Metal) there,
/// main.rs:7651-7653); `software` maps to `QT_QUICK_BACKEND=software`.
///
/// PARITY-DEBT #104 is CLOSED as of 2026-08-11: the crash auto-revert
/// sentinel and the frame-liveness watchdog now exist (`renderer_qt`). A
/// forced choice arms a sentinel here and the QML watchdog disarms it once
/// frames have genuinely been rendered; a launch that finds it still armed
/// reverts to "auto", so a backend this machine cannot start can no longer
/// lock the user out of Settings.
fn apply_renderer_preference() {
    // Do this FIRST, before any early return: a launch that died on a forced
    // backend must be undone even when this one is being overridden by env.
    let reverted = renderer_qt::revert_if_previous_launch_died();

    // Whitespace is not a backend choice. Qt itself does not trim every env
    // entry; normalize these before either the child or parent constructs Qt.
    #[cfg(target_os = "linux")]
    for name in ["QSG_RHI_BACKEND", "QT_QUICK_BACKEND"] {
        if std::env::var_os(name).is_some() && !qt_backend_env_overrides(name) {
            std::env::remove_var(name);
        }
    }

    if qt_backend_env_overrides("QSG_RHI_BACKEND") || qt_backend_env_overrides("QT_QUICK_BACKEND") {
        log::info!("[renderer] explicit Qt backend env present; leaving the choice to it");
        return;
    }
    let from_env = std::env::var("QBZ_RENDERER")
        .ok()
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty() && s != "auto");
    let choice = match from_env.clone() {
        Some(v) => v,
        // A revert has just rewritten the pref to "auto"; use that rather than
        // a value read one line before it changed.
        None if reverted => "auto".to_string(),
        None => settings_qt::pref_str("renderer", "auto"),
    };
    // Does this choice actually FORCE a backend? Only a forced one can brick a
    // startup, and only a forced one may arm the sentinel: arming for a choice
    // that changes nothing would let a crash from an unrelated cause silently
    // reset the user's renderer preference on the next launch.
    //
    // The GPU-tier aliases (auto / wgpu / gpu / hardware / hw) all resolve to
    // Qt's own default backend, so they force nothing — `wgpu` is a
    // cross-frontend name kept for the shared pref, not a Qt backend.
    let forces_backend = match choice.as_str() {
        "software" | "cpu" | "soft" => {
            std::env::set_var("QT_QUICK_BACKEND", "software");
            log::info!("[renderer] '{choice}' -> QT_QUICK_BACKEND=software");
            true
        }
        "gl" | "gles" | "femtovg" => {
            if cfg!(target_os = "macos") {
                log::info!("[renderer] '{choice}' on macOS -> Metal default (the Slint GL remap)");
                false
            } else if cfg!(target_os = "windows") {
                // d3d11 is Qt's healthy default here; desktop GL / ANGLE is a
                // downgrade, not the GPU tier this pref means. Same remap as
                // macOS: honour the intent (GPU path), not the literal backend.
                log::info!(
                    "[renderer] '{choice}' on Windows -> d3d11 default (the Slint GL remap)"
                );
                false
            } else {
                std::env::set_var("QSG_RHI_BACKEND", "opengl");
                log::info!("[renderer] '{choice}' -> QSG_RHI_BACKEND=opengl");
                true
            }
        }
        "auto" | "wgpu" | "gpu" | "hardware" | "hw" => {
            log::info!("[renderer] '{choice}' -> Qt default backend (GPU path)");
            false
        }
        other => {
            log::warn!("[renderer] unrecognized choice '{other}' -> Qt default backend");
            false
        }
    };
    // NOT armed for a `QBZ_RENDERER` override either: the revert rewrites the
    // PERSISTED pref, which an env override never set — it would blame (and
    // silently reset) a setting the user did not choose, while the env would
    // keep forcing the same dead backend on the next launch anyway.
    //
    // Arming AFTER `set_var` is still before anything reads it: Qt only
    // consults these when `QGuiApplication` builds the backend, which is the
    // caller's next statement.
    if forces_backend && from_env.is_none() {
        renderer_qt::arm_for_choice(&choice);
    } else {
        renderer_qt::clear_stale_sentinel();
        if from_env.is_some() && forces_backend {
            log::info!("[renderer] QBZ_RENDERER override — the startup sentinel stays out of it");
        }
    }
}

/// Cap glibc's malloc arenas before the first thread exists. Every thread
/// that first touches malloc gets its own 64 MB arena, and with ~50 threads
/// (tokio workers, Qt, PipeWire, zbus) the process held 82 of them: measured
/// 2026-09-02 on the owner's box, 257 MB of resident anonymous arena
/// mappings idle, against 71 MB with `MALLOC_ARENA_MAX=2` (RSS 728 -> 530
/// MB). Two arenas keep the audio thread and the rest from contending on one
/// lock; playback allocations are not on the hot path anyway. An explicit
/// `MALLOC_ARENA_MAX` in the environment wins — glibc already read it.
fn cap_malloc_arenas() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        const MALLOC_ARENA_MAX: &str = "MALLOC_ARENA_MAX";
        if std::env::var_os(MALLOC_ARENA_MAX).is_some() {
            log::info!("[qbz-qt] explicit {MALLOC_ARENA_MAX} preserved");
            return;
        }
        // SAFETY: mallopt only tunes allocator parameters; called on the main
        // thread before any other thread or allocator-dependent subsystem.
        let applied = unsafe { libc::mallopt(libc::M_ARENA_MAX, 2) } == 1;
        log::info!("[qbz-qt] malloc arenas capped at 2 (applied={applied})");
    }
}

/// Apply Settings > Appearance > Interface size through Qt's native high-DPI
/// path. This must run before `QGuiApplication::new()`: Qt then lays out the
/// scene in device-independent pixels and rasterizes text/vector content at
/// the effective DPR. A QML `scale` transform instead enlarges an already
/// rendered scene and produces the blurred text this setting exists to avoid.
fn apply_interface_scale_preference() {
    const QT_SCALE_FACTOR: &str = "QT_SCALE_FACTOR";
    let explicit = std::env::var_os(QT_SCALE_FACTOR)
        .map(|value| !value.to_string_lossy().trim().is_empty())
        .unwrap_or(false);
    if explicit {
        log::info!("[qbz-qt] explicit {QT_SCALE_FACTOR} preserved");
        return;
    }
    let factor = settings_qt::ui_scale_factor();
    std::env::set_var(QT_SCALE_FACTOR, factor.to_string());
    log::info!("[qbz-qt] native interface scale -> {factor} (QT_SCALE_FACTOR)");
}

// ============================ Shutdown guarantee ==========================

static OUTPUT_RELEASE_TASK: std::sync::Mutex<Option<std::thread::JoinHandle<Result<(), String>>>> =
    std::sync::Mutex::new(None);

/// One-shot latch for [`arm_hard_exit_watchdog`].
static HARD_EXIT_ARMED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Hard-exit watchdog — the guarantee that QUITTING ALWAYS ENDS THE PROCESS.
///
/// Owner-reported three times (2026-08-04): "Quit QBZ" closed the window and
/// left the process alive, with no way out but `kill`. Root cause, reproduced
/// under `QT_QPA_PLATFORM=vnc` the same day: in Qt 6, `Qt.quit()` delivers
/// `QEvent::Quit`, and `QGuiApplication::event` first tries to CLOSE every
/// top-level window — any window refusing its close event silently CANCELS
/// the whole quit (qtbase `QGuiApplicationPrivate::tryCloseAllWindows`). Two
/// windows here refuse: Main.qml's close-to-tray arm (whenever
/// `trayLive && closeToTray`) and MiniWindow.qml's `onClosing`
/// (unconditionally — its close means "exit the mini", never "quit the app").
/// The log signature of the bug: `[tray] quit requested` and the QML
/// handler's `Qt.quit()` both fire, and `event loop exited` never appears.
///
/// The FIX is that every quit path now calls `Qt.exit(0)`, which exits the
/// event loops directly with no window veto. THIS watchdog is the backstop
/// behind it: armed at the moment quit is REQUESTED (and again, idempotently,
/// when the loop exits), it hard-terminates the process if any later stage —
/// signal delivery, QML, the loop itself, the cast shutdown, a Qt/QML/Wayland
/// destructor, an atexit handler — wedges.
///
/// `libc::_exit` on purpose: `std::process::exit` runs libc `exit()`, whose
/// atexit chain is where Qt's `Q_GLOBAL_STATIC` destructors live — a hang
/// there is one of the wedges being guarded against — and `abort()` would
/// SIGABRT/core-dump a healthy shutdown.
///
/// SAFE BY DESIGN, not by luck: everything the app owes the user is already
/// on disk before any quit path arms this — the geometry flushes in the QML
/// quit handlers BEFORE `Qt.exit(0)`, prefs/settings persist on change, and
/// the stores write at mutation time. Skipping C++ teardown loses nothing the
/// user can observe. Do NOT "clean this up" into a graceful join or remove
/// the `_exit`: an app the owner cannot close is the bug this ends, and it
/// already survived one round of fixes.
pub(crate) fn arm_hard_exit_watchdog(source: &'static str) {
    use std::sync::atomic::Ordering;
    if HARD_EXIT_ARMED.swap(true, Ordering::SeqCst) {
        return;
    }
    log::info!(
        "[shutdown] hard-exit watchdog armed ({source}); force-exit in 5s if shutdown wedges"
    );
    let _ = std::thread::Builder::new()
        .name("qbz-exit-watchdog".into())
        .spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(5));
            log::error!(
                "[shutdown] quit did not complete within 5s of {source}; forcing _exit(0). \
                 The last '[shutdown]' line above this one names the statement that wedged. \
                 (All user state was persisted before the quit was requested.)"
            );
            log::logger().flush();
            // SAFETY: `_exit` terminates the process immediately, without
            // running atexit handlers or destructors — that is the point;
            // see the header above.
            unsafe { libc::_exit(0) };
        });
    // Send release at the first quit request, even if Qt never exits its loop.
    // The terminal player fence also rejects late Connect/Cast continuations.
    if let Some(runtime) = APP.get() {
        let player = runtime.core().player();
        player.begin_shutdown();
        playback_qt::cancel_owner_playback_tasks();
        *OUTPUT_RELEASE_TASK.lock().unwrap() = Some(std::thread::spawn(move || player.release_device()));
    }
}

/// Is THIS process one of the internal, disposable child processes that
/// re-enter `main` (presentation preflight, GPU preflight)?
///
/// Read before the logger is installed, from env markers only, so a child
/// never opens the on-disk log: `qbz_log::install` rotates `qbz.log` to
/// `qbz.log.prev` and starts an empty one, which mid-run renames the LIVE log
/// away from the parent. That is #749's second finding — the reporter's
/// `qbz.log` held two lines on every run (the child's own first two, since it
/// inherits the QT_SCALE_FACTOR the parent just set) while the parent's whole
/// log sat unread in `qbz.log.prev`.
fn is_internal_child_process() -> bool {
    #[cfg(target_os = "linux")]
    if renderer_qt::auto_preflight::child_requested() {
        return true;
    }
    renderer_qt::gpu_preflight_child_requested()
}

fn main() {
    if is_internal_child_process() {
        qbz_log::install_without_file_sink("info");
    } else {
        qbz_log::install("info");
    }
    // Immediately after the logger, so every later frame is covered — the Qt
    // and QML construction window included. See qbz-log/src/fatal.rs for the
    // intermittent packaged-AppDir SIGSEGV this exists to name.
    qbz_log::install_fatal_signal_reporter();
    // Declared first so normal/early returns flush after all later destructors,
    // including the final summary of a consecutive logging burst.
    struct FlushLogsOnExit;
    impl Drop for FlushLogsOnExit {
        fn drop(&mut self) {
            log::logger().flush();
        }
    }
    let _flush_logs = FlushLogsOnExit;
    cap_malloc_arenas();
    // Before even the disposable GPU-preflight QGuiApplication. A child
    // spawned later inherits the resolved factor and takes the same path.
    apply_interface_scale_preference();
    #[cfg(target_os = "linux")]
    if renderer_qt::auto_preflight::child_requested() {
        let app = QGuiApplication::new();
        let result = renderer_qt::auto_preflight::run_child();
        drop(app);
        log::logger().flush();
        std::process::exit(result);
    }
    // Internal, disposable graphics child. It must branch before the normal
    // instance lock, navigation crash-chain, runtime and audio thread: a GPU
    // whose DMA-BUFs the active Wayland compositor rejects is expected to kill
    // THIS process, while the waiting parent remains healthy and falls back to
    // Auto.
    if renderer_qt::gpu_preflight_child_requested() {
        let app = QGuiApplication::new();
        let result = renderer_qt::run_gpu_preflight_child();
        drop(app);
        log::logger().flush();
        std::process::exit(result);
    }
    deep_link_qt::capture_argv();
    #[cfg(target_os = "linux")]
    if !single_instance_qt::acquire_or_raise() {
        log::info!("[qbz-qt] another instance owns the session bus name; exiting");
        return;
    }
    // Validate a persisted explicit GPU before constructing anything costly or
    // user-visible. The child uses its own Wayland connection; fatal protocol
    // errors cannot take this parent down.
    renderer_qt::preflight_saved_gpu_at_boot();
    #[cfg(target_os = "linux")]
    apply_renderer_preference();
    #[cfg(target_os = "linux")]
    if let Err(reason) = renderer_qt::auto_preflight::at_boot() {
        log::error!("[renderer] startup stopped before runtime/Qt: {reason}");
        log::logger().flush();
        std::process::exit(78);
    }
    // Same decision, different primitive: a per-session named mutex arbitrates
    // and a named pipe carries the handoff. `acquire_or_raise` has already
    // forwarded this launch's deep link (or a bare PRESENT) by the time it
    // answers false, so there is nothing left to do but leave.
    #[cfg(target_os = "windows")]
    if !single_instance_win::acquire_or_raise() {
        log::info!("[qbz-qt] another instance owns the mutex; forwarded and exiting");
        return;
    }
    // Link anchor for the hand-written QAbstractListModel. Its QML singleton
    // registration itself runs at QCoreApplication startup.
    local_tracks_model_qt::register_qml_model();
    local_albums_model_qt::register_qml_model();
    local_artists_model_qt::register_qml_model();
    player_bridge::register_seek_waveform_qml_item();
    // BEFORE any restore reads a pref: read + increment the crash chain, so a
    // boot that dies from a restored view or queue degrades the NEXT boot
    // instead of trapping the user in a loop it cannot escape from inside the
    // UI. Cleared by `mark_startup_healthy` once the shell is usable.
    nav_qt::arm_startup_probe();
    // Hand the level to the shared persistence module: the queue restore has
    // its own rung on the same ladder (>=3 skips it for this boot only).
    qbz_app::session_persist::set_crash_chain_level(nav_qt::crash_level());
    // glibc resolver hardening (2026-08-10): this host's path to its first
    // resolver drops the SECOND of two parallel DNS queries on one socket,
    // so every getaddrinfo (which fires A+AAAA in parallel) stalls for the
    // full 5s RES_TIMEOUT before retrying — measured: init-segment fetches
    // with send(dns+connect+tls+ttfb)=5449ms, body=0ms after the app idled.
    // `single-request` serializes A/AAAA (sequential queries survive the
    // drop: 0.19s vs 8.07s, measured while the drop was active), at the cost
    // of one extra RTT per fresh lookup. Process-local, honored by glibc at
    // resolver init — must be set before ANY name resolution, hence first
    // in main. No-op on musl/BSD/macOS resolvers.
    if std::env::var_os("RES_OPTIONS").is_none() {
        std::env::set_var("RES_OPTIONS", "single-request");
        log::info!("[qbz-qt] RES_OPTIONS=single-request set (parallel A/AAAA drop workaround)");
    }
    // i18n (phase 20): honor the persisted ui_prefs language; "auto"/missing
    // resolves POSIX env (qbz_i18n::resolve_auto).
    let boot_lang = settings_qt::pref_str("language", "auto");
    qbz_i18n::set_language(if boot_lang == "auto" {
        qbz_i18n::resolve_auto()
    } else {
        boot_lang.as_str()
    });
    // Kiosk profile (2026-08-02 kiosk-port contract §8.2/§8.3): QBZ_PROFILE
    // wins, else the persisted ui_prefs.profile key. Resolved HERE, before the
    // bridges are constructed, because QbzShell seeds both `kiosk_profile` and
    // `reduce_motion` from it at construction (shell_bridge.rs) and the
    // post-login screen writes read it (:489, :565).
    kiosk_profile_qt::init_at_boot();

    // rustls process-level CryptoProvider (aws-lc-rs) — required before any
    // reqwest call, same as the Slint and daemon binaries.
    qbz_app::ensure_crypto_provider();

    // Eight workers instead of one per core: the async lane carries API
    // calls, catalog queries and UI fan-out, none of it CPU-bound, while audio
    // runs on its own threads. Fewer workers = fewer idle wakeups and fewer
    // malloc arenas (see `cap_malloc_arenas`). The blocking pool keeps its
    // own default sizing.
    let tokio_runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(8)
        .thread_name("tokio-rt-worker")
        .enable_all()
        .build()
        .expect("failed to build the tokio runtime");
    let _ = TOKIO.set(tokio_runtime);

    // `with_visualizer` == `new` plus a VisualizerTap wired into the player.
    // The tap starts DISABLED (it captures nothing and the FFT producer idles),
    // so this costs nothing until the Large dock's band is shown. It is a
    // read-only copy downstream of the bit-perfect stream — no device/stream
    // init is touched.
    let runtime = Arc::new(AppRuntime::with_visualizer(LoggingAdapter::new("[qbz-qt]")));
    if let Some(tap) = runtime.visualizer_tap().cloned() {
        viz_qt::install(tap);
        viz_qt::set_mode(settings_qt::large_spectrum_mode());
    } else {
        log::warn!("[qbz-qt] runtime has no visualizer tap; the Large dock band stays empty");
    }
    // MusicBrainz cache — a SQLite store at
    // <data-dir>/qbz/cache/musicbrainz_cache.db so artist metadata,
    // relationships and scene discovery persist across sessions.
    //
    // THIS WAS MISSING ENTIRELY. `set_musicbrainz_cache` had exactly one
    // caller in the tree — the Slint binary (crates/qbz/src/main.rs:9011) —
    // so every MusicBrainz call this build made went uncached, on every
    // artist page, against an API with a strict rate limit. It is the same
    // path and the same filename the reference uses, so a user switching
    // between the two builds keeps one warm cache.
    //
    // Failure to open only degrades to direct network calls: the core's
    // methods skip the cache when none is set.
    if let Some(data_dir) = dirs::data_dir() {
        let cache_dir = data_dir.join("qbz").join("cache");
        if let Err(e) = std::fs::create_dir_all(&cache_dir) {
            log::warn!("[qbz-qt] MB cache dir create failed: {e}");
        } else {
            let db_path = cache_dir.join("musicbrainz_cache.db");
            match qbz_integrations::musicbrainz::cache::MusicBrainzCache::new(&db_path) {
                Ok(cache) => {
                    runtime.core().set_musicbrainz_cache(cache);
                    log::info!("[qbz-qt] MB cache opened at {db_path:?}");
                }
                Err(e) => log::warn!("[qbz-qt] MB cache open failed: {e}"),
            }
        }
    }

    let _ = APP.set(runtime);

    // Renderer policy stays before QGuiApplication. Scroll policy only needs
    // to precede Flickable construction. Preferred GPU has a narrower safe
    // slot: QGuiApplication must first install the platform integration so we
    // can enumerate Qt's real QVulkanInstance, but the selection env must land
    // before the first QQuickWindow/QRhi is constructed.
    #[cfg(not(target_os = "linux"))]
    apply_renderer_preference();

    // FONT ENGINE (Windows). Qt's default rasteriser on Windows renders the
    // lyric text thin and jagged - the owner's words were "como fuente de
    // bloc de notas con extra alias", and the same text is clean under
    // FreeType. Verified on the box by launching with
    // QT_QPA_PLATFORM=windows:fontengine=freetype and comparing the lyrics
    // panel: same build, same font, only the rasteriser changed.
    //
    // Only when the variable carries no real value. `offscreen` is what the
    // release smoke gate passes here, and overwriting it would make the gate
    // test a platform nobody ships. Empty counts as absent: a cleared
    // variable is present-with-no-value, which `var_os().is_some()` reports
    // as set (the same trap the renderer preference had).
    #[cfg(windows)]
    {
        let explicit = std::env::var_os("QT_QPA_PLATFORM")
            .map(|v| !v.to_string_lossy().trim().is_empty())
            .unwrap_or(false);
        if !explicit {
            std::env::set_var("QT_QPA_PLATFORM", "windows:fontengine=freetype");
            log::info!("[qbz-qt] font engine -> freetype (Qt's Windows rasteriser aliases text)");
        }
    }

    // The process identity Windows groups the taskbar button under, and the
    // one a toast's `app_id` is matched against. Must be set BEFORE any window
    // exists -- Windows caches the identity with the first one, and a later
    // call is documented to fail.
    //
    // Without it the taskbar treats each launch as an anonymous window and the
    // MSI's Start-menu shortcut (which carries System.AppUserModel.ID) does not
    // group with it, and toasts are dropped with no error at all.
    #[cfg(target_os = "windows")]
    {
        use windows_sys::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
        let id: Vec<u16> = "com.blitzfc.qbz\0".encode_utf16().collect();
        // SAFETY: `id` is NUL-terminated UTF-16 and outlives the call, which
        // copies it. Called before any window is created, as documented.
        let hr = unsafe { SetCurrentProcessExplicitAppUserModelID(id.as_ptr()) };
        if hr < 0 {
            // Worth a line: when this fails, taskbar grouping and toast
            // delivery both break, and both fail SILENTLY. Without this the
            // only symptom is "notifications do not work" with nothing to
            // point at.
            log::warn!("[qbz-qt] SetCurrentProcessExplicitAppUserModelID failed: 0x{hr:08X}");
        }
    }

    // D7 (research/02 §4.4): with no explicit style Windows loads its native
    // Controls style, and popups without an explicit `background:` inherit
    // native chrome. Pin Basic on Windows so windeployqt ships the style the
    // QML was verified against. Preserve Qt's existing platform defaults on
    // Linux and macOS. This must run before the first Controls import.
    #[cfg(target_os = "windows")]
    cxx_qt_lib::QQuickStyle::set_style(&QString::from("Basic"));

    let mut app = QGuiApplication::new();
    // The Wayland `app_id` (and the D-Bus notification `desktop-entry` hint)
    // comes from this, and with nothing set Qt sends the binary's name, `qbz`.
    // GNOME Shell matches a live window to its launcher by that id against the
    // installed `com.blitzfc.qbz.desktop`, so the dock painted the generic
    // icon (and the notification badge attached to nothing) while the app
    // grid, which reads the .desktop directly, showed the right one.
    // `StartupWMClass` covers X11 only. Verified on an Ubuntu GNOME/Wayland VM
    // 2026-09-11: a `qbz.desktop` symlink made the dock icon appear; this is
    // the real fix. Every Linux package installs the entry under this id
    // (deb/rpm, AUR, Flatpak, Snap).
    #[cfg(target_os = "linux")]
    QGuiApplication::set_desktop_file_name(&QString::from("com.blitzfc.qbz"));
    renderer_qt::apply_gpu_preference();

    // W25. Windows asks WM_QUERYENDSESSION before it decides to log off or
    // shut down, and Qt turns that into `commitDataRequest` and BLOCKS inside
    // the signal until the handler returns -- the one moment where a
    // synchronous persist is safe. Anything deferred to a queued connection or
    // another thread races the process being killed.
    //
    // WINDOWS ONLY, deliberately, though the hook itself is portable. Qt's
    // session manager exists on X11 too, so installing this everywhere would
    // add a blocking save to the Linux logout path -- a behaviour change on the
    // platform this project leads with, and one that can only ever delay a
    // logout. W25 is a Windows contract item; Linux can opt in on its own
    // terms, deliberately, rather than inheriting it from a port.
    #[cfg(target_os = "windows")]
    win_shell::install_commit_data_handler(on_session_commit_data);

    // W27's other half. Main.qml asks for CustomizeWindowHint so Qt does not
    // paint its own button cluster over the QBZ header; this gives the window
    // back a hit test, which that hint otherwise answers as HTNOWHERE for
    // every pixel.
    #[cfg(target_os = "windows")]
    win_shell::install_hittest_filter();
    let mut engine = QQmlApplicationEngine::new();

    // ── THE APP TYPEFACE, AND WHY IT IS SET RIGHT HERE ────────────────────
    //
    // Settings > Appearance > Typography & Language > Font. This is the only
    // place it CAN be applied, and the ordering below is the whole feature:
    //
    //   1. FontPreload.qml registers the bundled faces. A family does not
    //      exist for Qt until something loads its file, and `load()` returns
    //      with its FontLoaders already in the font database.
    //   2. The application font is set from the persisted choice. A plain
    //      `Text` reads the application font when it is CONSTRUCTED — it does
    //      not follow ApplicationWindow.font (Controls only), and it ignores
    //      a later QGuiApplication::setFont. Both measured against this Qt
    //      build; the evidence is written up in qml/FontPreload.qml.
    //   3. Main.qml builds the UI, and every one of its 1082 Text items is
    //      born with the chosen face.
    //
    // Swap any two of those steps and the setting silently does nothing,
    // which is exactly how it shipped the first time.
    //
    // "System" resolves to "" and takes no branch at all, so a user who never
    // opens the row gets precisely the font this app rendered before the
    // setting existed.
    if let Some(engine) = engine.as_mut() {
        engine.load(&QUrl::from(
            "qrc:/qt/qml/com/blitzfc/qbz/qml/FontPreload.qml",
        ));
    }
    font_qt::register_script_fallbacks();
    let app_font_family = settings_qt::app_font_family();
    if !app_font_family.is_empty() {
        if let Some(app) = app.as_mut() {
            let mut font = cxx_qt_lib::QFont::default();
            font.set_family(&QString::from(app_font_family.as_str()));
            app.set_application_font(&font);
            log::info!("[qbz-qt] app font -> {app_font_family}");
        }
    }

    if let Some(engine) = engine.as_mut() {
        engine.load(&QUrl::from("qrc:/qt/qml/com/blitzfc/qbz/qml/Main.qml"));
    }

    if let Some(app) = app.as_mut() {
        app.exec();
        // The event loop is gone: from here on the process has no UI and the
        // user cannot do anything but wait, so NOTHING on this path may block
        // without a bound.
        log::info!("[qbz-qt] event loop exited; shutting down");
        // The catalog worker commits at most its current bounded batch, whose
        // checkpoint is part of that same transaction. A later launch resumes.
        local_catalog_qt::cancel();
        // A Plex page and its checkpoint are one transaction. Cancellation
        // prevents section prune and the next launch resumes at that page.
        local_plex::cancel_sync();
        // Jellyfin sync/hydration responses carry an epoch; invalidating it
        // here prevents a late server response from writing after UI teardown.
        media_sync_qt::cancel_all();
        // The backstop for everything below AND for Qt's own teardown (engine
        // + QGuiApplication destructors, atexit chain): if any of it wedges,
        // the watchdog `_exit(0)`s the process. Idempotent — the quit paths
        // arm it earlier, at the moment quit was requested.
        arm_hard_exit_watchdog("event-loop exit");
        local_restore_qt::save_session_on_exit();
        // Same reason as logout: leaving the app must stop every renderer.
        // QConnect goes first logically (its future withdraws LAN admission
        // before touching authority), while the independent notification and
        // cast cleanup run concurrently under the existing five-second hard
        // exit budget. Construct every timer inside the runtime: constructing
        // `tokio::time::timeout` before `block_on` has no reactor and panics.
        let mut qconnect_owner_safe = true;
        if let Some(rt) = TOKIO.get() {
            let (qconnect_stopped, notification_withdrawn, cast_stopped) = rt.block_on(async {
                let qconnect_stop = async {
                    let Some(service) = qconnect_qt::service() else {
                        return true;
                    };
                    match tokio::time::timeout(
                        std::time::Duration::from_secs(3),
                        service.disconnect_for_shutdown(),
                    )
                    .await
                    {
                        Ok(Ok(can_persist_owner)) => can_persist_owner,
                        Ok(Err(error)) => {
                            log::warn!("[qbz-qt] QConnect shutdown failed: {error}");
                            false
                        }
                        Err(_) => false,
                    }
                };
                let notification_stop = async {
                    tokio::time::timeout(
                        std::time::Duration::from_secs(1),
                        qbz_media_controls::withdraw_track_notification(),
                    )
                    .await
                    .is_ok()
                };
                let cast_stop = async {
                    tokio::time::timeout(
                        std::time::Duration::from_secs(2),
                        cast_qt::service().shutdown(),
                    )
                    .await
                    .is_ok()
                };
                tokio::join!(qconnect_stop, notification_stop, cast_stop)
            });
            qconnect_owner_safe = qconnect_stopped;
            if !qconnect_stopped {
                log::warn!(
                    "[qbz-qt] preserving the saved owner session after QConnect shutdown \
                     (teardown incomplete or owner playback intentionally not restored)"
                );
            }
            if !notification_withdrawn {
                log::warn!(
                    "[qbz-qt] notification withdrawal did not finish within 1s; exiting anyway"
                );
            }
            if !cast_stopped {
                log::warn!(
                    "[qbz-qt] cast shutdown did not finish within 2s; exiting anyway \
                     (a cast device may keep playing until it times out)"
                );
            }
        }

        if let Some(task) = OUTPUT_RELEASE_TASK.lock().unwrap().take() {
            match task.join() {
                Ok(Ok(())) => log::info!("[shutdown] output-device release acknowledged"),
                Ok(Err(error)) => log::error!("[shutdown] output-device release failed: {error}"),
                Err(_) => log::error!("[shutdown] output-device release task panicked"),
            }
        }

        // Final full snapshot. QConnect teardown MUST precede this write: a
        // delegated queue is ephemeral and may never become the owner's saved
        // session. If bounded teardown failed, preserving the previous saved
        // snapshot is safer than writing guest state.
        if qconnect_owner_safe {
            qbz_app::session_persist::save_on_exit();
        }
        // Listen log: close the row in flight as `shutdown`. Same placement
        // and the same reasoning as the session flush above (one SQLite
        // write on the main thread, behind the watchdog).
        listen_log_qt::shutdown_blocking();
        local_service_qt::reset();
        log::info!("[qbz-qt] shutdown complete");
    }
    // Explicit, INSTRUMENTED drops (2026-08-04 quit incident). Rust would run
    // these same two destructors implicitly in this same order — the point of
    // spelling them out is the log lines: if a Qt/QML/Wayland destructor ever
    // wedges again, the LAST line printed names the exact statement, instead
    // of a silent ghost process nobody can diagnose. The watchdog armed above
    // still ends the process either way. KEEP these lines — they are cheap,
    // and this bug already cost multiple rounds.
    log::info!("[shutdown] dropping the QML engine");
    drop(engine);
    log::info!("[shutdown] engine down; dropping QGuiApplication");
    drop(app);
    log::info!("[shutdown] QGuiApplication down; returning from main");
}
