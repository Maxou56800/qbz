//! Framework-agnostic application runtime facade.
//!
//! [`AppRuntime`] is the composition root that a native UI shell (Qt, TUI,
//! headless) builds on. It owns an `Arc<QbzCore<A>>`, the framework-
//! agnostic runtime state machine, and the per-user session, all without any
//! Tauri dependency.
//!
//! Session activation is deliberately minimal: it opens the session store and
//! performs the portable scaffolding (user paths, directories, last-user
//! marker, runtime state). The shell binds its additional per-user stores as
//! each session comes online.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use qbz_audio::{settings::AudioSettingsStore, AudioDiagnostic, AudioSettings, VisualizerTap};
use qbz_core::{FrontendAdapter, QbzCore};
use qbz_offline_cache::OfflineCacheState;
use qbz_player::Player;
use tokio::task::JoinHandle;

use crate::runtime::RuntimeManager;
use crate::session_store::SessionStore;
use crate::subscription_lifecycle;
use crate::user_data::UserDataPaths;

/// How often an activated session re-checks the subscription grace clock,
/// so a long-running session without a re-login still crosses the 30-day
/// line on time.
const SUBSCRIPTION_TICK: Duration = Duration::from_secs(24 * 60 * 60);

/// The per-user stores opened for the currently active session.
///
/// Minimal by design: it holds only the session store. A shell opens further
/// per-user stores as the views that need them come online, rather than
/// loading the full WebKit-era store set up front.
struct ActiveSession {
    user_id: u64,
    data_dir: PathBuf,
    session_store: SessionStore,
}

/// The offline cache a host registered, shared with the subscription ticker.
type OfflineCacheSlot = Arc<Mutex<Option<Arc<OfflineCacheState>>>>;

/// Composition root for a non-Tauri UI shell.
///
/// Generic over the [`FrontendAdapter`] the shell supplies, so the same
/// facade serves a Slint adapter, a TUI adapter, or a headless one.
pub struct AppRuntime<A: FrontendAdapter + Send + Sync + 'static> {
    core: Arc<QbzCore<A>>,
    runtime: Arc<RuntimeManager>,
    user_paths: UserDataPaths,
    session: Mutex<Option<ActiveSession>>,
    /// The visualizer tap handed to the player, retained so the shell can start
    /// the FFT producer and toggle capture. `None` for shells that do not drive
    /// audio visualization (the default `new`/`with_audio_settings` path).
    visualizer_tap: Option<VisualizerTap>,
    /// The host's offline cache, if it has one (Qt does, qbzd does not).
    /// The subscription lifecycle purges through it once the grace window
    /// has passed; without one the verdict is still recorded.
    offline_cache: OfflineCacheSlot,
    /// The daily subscription re-check for the active session.
    subscription_ticker: Mutex<Option<JoinHandle<()>>>,
}

impl<A: FrontendAdapter + Send + Sync + 'static> AppRuntime<A> {
    /// Build with explicit audio settings.
    ///
    /// Performs no disk or network access — used by tests and by shells that
    /// already have audio settings loaded.
    pub fn with_audio_settings(
        adapter: A,
        device_name: Option<String>,
        audio_settings: AudioSettings,
        visualizer_tap: Option<VisualizerTap>,
    ) -> Self {
        let host = qbz_core::system_capabilities::memory_profile();
        qbz_player::player::apply_memory_tuning(
            host.class == qbz_core::system_capabilities::MemoryClass::LowMemory,
            host.audio_cache_l1_max_bytes,
            host.max_initial_buffer_bytes,
        );
        let diagnostic = AudioDiagnostic::new();
        let player = Player::new(device_name, audio_settings, visualizer_tap, diagnostic);
        let core = QbzCore::new(adapter, player);
        Self {
            core: Arc::new(core),
            runtime: Arc::new(RuntimeManager::new()),
            user_paths: UserDataPaths::new(),
            session: Mutex::new(None),
            visualizer_tap: None,
            offline_cache: Arc::new(Mutex::new(None)),
            subscription_ticker: Mutex::new(None),
        }
    }

    /// Build, loading persisted audio settings from [`AudioSettingsStore`].
    ///
    /// Falls back to defaults when no settings are saved or the store cannot
    /// be opened. This mirrors the recipe in the Tauri `CoreBridge::new`.
    /// It does not touch the network — call [`AppRuntime::init`] for that.
    pub fn new(adapter: A) -> Self {
        let (device_name, audio_settings) = AudioSettingsStore::new()
            .ok()
            .and_then(|store| {
                store
                    .get_settings()
                    .ok()
                    .map(|settings| (settings.output_device.clone(), settings))
            })
            .unwrap_or_else(|| {
                log::info!("[AppRuntime] No saved audio settings, using defaults");
                (None, AudioSettings::default())
            });
        Self::with_audio_settings(adapter, device_name, audio_settings, None)
    }

    /// Build like [`AppRuntime::new`], but also wire a [`VisualizerTap`] into the
    /// player and retain it so the shell can start the frontend-agnostic FFT
    /// producer ([`qbz_audio::visualizer::spawn_visualizer_thread`]) and toggle
    /// capture via the tap's `set_enabled`. Used by the Qt shell for the
    /// immersive-view audio visualizers. The tap starts disabled, so it adds
    /// no runtime cost until the immersive view enables it.
    pub fn with_visualizer(adapter: A) -> Self {
        let (device_name, audio_settings) = AudioSettingsStore::new()
            .ok()
            .and_then(|store| {
                store
                    .get_settings()
                    .ok()
                    .map(|settings| (settings.output_device.clone(), settings))
            })
            .unwrap_or_else(|| {
                log::info!("[AppRuntime] No saved audio settings, using defaults");
                (None, AudioSettings::default())
            });
        let tap = VisualizerTap::new();
        let mut rt =
            Self::with_audio_settings(adapter, device_name, audio_settings, Some(tap.clone()));
        rt.visualizer_tap = Some(tap);
        rt
    }

    /// The visualizer tap, if this runtime was built with [`AppRuntime::with_visualizer`].
    pub fn visualizer_tap(&self) -> Option<&VisualizerTap> {
        self.visualizer_tap.as_ref()
    }

    /// Initialize the core (extracts Qobuz bundle tokens).
    ///
    /// Best-effort and offline-tolerant: a network failure here leaves the
    /// core usable for local/offline playback, matching [`QbzCore::init`].
    pub async fn init(&self) -> Result<(), String> {
        self.core.init().await.map_err(|e| e.to_string())
    }

    /// The orchestrator. Shells reach catalog, playback, queue, and auth
    /// functionality through this handle.
    pub fn core(&self) -> &Arc<QbzCore<A>> {
        &self.core
    }

    /// The framework-agnostic runtime state machine.
    pub fn runtime(&self) -> &Arc<RuntimeManager> {
        &self.runtime
    }

    // ==================== Session activation (Task 2) ====================

    /// Activate the per-user session against explicit directories.
    ///
    /// This is the testable core of session activation. It creates the
    /// directories, opens the session store, and marks the runtime state
    /// machine as session-activated. It performs no global-path writes (no
    /// `last_user_id` marker) and does not touch [`UserDataPaths`] state, so
    /// tests and shells managing their own paths can call it directly.
    pub async fn activate_at(
        &self,
        user_id: u64,
        data_dir: &Path,
        cache_dir: &Path,
    ) -> Result<(), String> {
        std::fs::create_dir_all(data_dir)
            .map_err(|e| format!("Failed to create user data dir: {}", e))?;
        std::fs::create_dir_all(cache_dir)
            .map_err(|e| format!("Failed to create user cache dir: {}", e))?;

        let session_store = SessionStore::new_at(data_dir)?;

        self.runtime.set_session_activated(true, user_id).await;

        {
            let mut guard = self
                .session
                .lock()
                .map_err(|e| format!("session lock poisoned: {}", e))?;
            *guard = Some(ActiveSession {
                user_id,
                data_dir: data_dir.to_path_buf(),
                session_store,
            });
        }

        log::info!("[AppRuntime] Session activated for user");

        // Subscription lifecycle (D4): record the login verdict from the
        // core's session — a member without the offline entitlement starts
        // the grace clock, a subscriber clears it — and purge the offline
        // cache when the window has passed. Shared by every host through
        // this one activation path; best-effort, never blocks entry.
        self.run_subscription_lifecycle().await;
        self.start_subscription_ticker(data_dir.to_path_buf());
        Ok(())
    }

    /// Register (or clear) the host's offline cache. When a session is
    /// already active and a cache arrives, the grace clock is enforced
    /// against it right away — Qt activates its cache after the session.
    pub fn set_offline_cache(self: &Arc<Self>, cache: Option<Arc<OfflineCacheState>>) {
        let registered = cache.is_some();
        if let Ok(mut slot) = self.offline_cache.lock() {
            *slot = cache;
        }
        if registered && self.is_session_active() {
            let runtime = Arc::clone(self);
            tokio::spawn(async move { runtime.run_subscription_lifecycle().await });
        }
    }

    async fn run_subscription_lifecycle(&self) {
        let Some(data_dir) = self.active_data_dir() else {
            return;
        };
        let session = {
            let client_lock = self.core.client();
            let guard = client_lock.read().await;
            match guard.as_ref() {
                Some(client) => client.session().await,
                None => None,
            }
        };
        let offline = self.offline_cache.lock().ok().and_then(|slot| slot.clone());
        let now = subscription_lifecycle::now_unix_secs();
        match subscription_lifecycle::run(&data_dir, session.as_ref(), offline, now).await {
            Ok(v) => log::info!(
                "[AppRuntime] subscription verdict: offline_entitled={} member_only={} invalid_since={:?} purged={}",
                v.offline_entitled,
                v.member_only,
                v.invalid_since,
                v.purged
            ),
            Err(e) => log::error!("[AppRuntime] subscription lifecycle failed: {e}"),
        }
    }

    fn start_subscription_ticker(&self, data_dir: PathBuf) {
        let offline_cache = Arc::clone(&self.offline_cache);
        let handle = tokio::spawn(async move {
            let mut interval = tokio::time::interval(SUBSCRIPTION_TICK);
            interval.tick().await; // the activation pass already ran
            loop {
                interval.tick().await;
                let offline = offline_cache.lock().ok().and_then(|slot| slot.clone());
                let now = subscription_lifecycle::now_unix_secs();
                if let Err(e) = subscription_lifecycle::run(&data_dir, None, offline, now).await {
                    log::error!("[AppRuntime] subscription tick failed: {e}");
                }
            }
        });
        if let Ok(mut ticker) = self.subscription_ticker.lock() {
            if let Some(previous) = ticker.replace(handle) {
                previous.abort();
            }
        }
    }

    fn stop_subscription_ticker(&self) {
        if let Ok(mut ticker) = self.subscription_ticker.lock() {
            if let Some(handle) = ticker.take() {
                handle.abort();
            }
        }
    }

    fn active_data_dir(&self) -> Option<PathBuf> {
        self.session
            .lock()
            .ok()
            .and_then(|guard| guard.as_ref().map(|s| s.data_dir.clone()))
    }

    /// Activate the per-user session for `user_id`.
    ///
    /// Resolves the real per-user directories through [`UserDataPaths`],
    /// activates against them, and persists the last-user marker so the
    /// session can be restored on the next launch.
    pub async fn activate(&self, user_id: u64) -> Result<(), String> {
        Self::adopt_guest_profile(user_id);
        self.user_paths.set_user(user_id);
        let data_dir = self.user_paths.user_data_dir()?;
        let cache_dir = self.user_paths.user_cache_dir()?;
        self.activate_at(user_id, &data_dir, &cache_dir).await?;
        UserDataPaths::save_last_user_id(user_id)?;
        Ok(())
    }

    /// First real login takes possession of the guest profile (#553): data
    /// built while logged off (local library, mixtapes, per-user prefs —
    /// `users/0/`) is renamed to this account IF the account has no profile
    /// on this machine yet. An existing profile always wins — the guest
    /// dirs then stay parked at user 0, no merge is attempted. Best-effort:
    /// a failed rename logs and falls through to a fresh profile; login
    /// must never break here.
    fn adopt_guest_profile(user_id: u64) {
        if user_id == 0 {
            return;
        }
        for (kind, guest, target) in [
            (
                "data",
                UserDataPaths::data_dir_for(0),
                UserDataPaths::data_dir_for(user_id),
            ),
            (
                "cache",
                UserDataPaths::cache_dir_for(0),
                UserDataPaths::cache_dir_for(user_id),
            ),
        ] {
            let (Ok(guest), Ok(target)) = (guest, target) else {
                continue;
            };
            if guest.is_dir() && !target.exists() {
                match std::fs::rename(&guest, &target) {
                    Ok(()) => {
                        log::info!("[AppRuntime] guest profile {kind} adopted by the new session")
                    }
                    Err(e) => log::warn!(
                        "[AppRuntime] guest profile {kind} adoption failed ({e}); starting fresh"
                    ),
                }
            }
        }
    }

    /// Activate an offline-only session using the last known user.
    ///
    /// Falls back to user id `0` (an empty profile) when no previous session
    /// was recorded. Does not re-persist the last-user marker.
    pub async fn activate_offline(&self) -> Result<(), String> {
        let user_id = UserDataPaths::load_last_user_id().unwrap_or(0);
        self.user_paths.set_user(user_id);
        let data_dir = self.user_paths.user_data_dir()?;
        let cache_dir = self.user_paths.user_cache_dir()?;
        self.activate_at(user_id, &data_dir, &cache_dir).await
    }

    /// Deactivate the current session.
    ///
    /// Drops the open per-user stores (closing their database connections),
    /// clears the active user, and resets the runtime state machine. The
    /// `last_user_id` marker is intentionally kept on disk so a later
    /// offline session can still find the user's data.
    pub async fn deactivate(&self) -> Result<(), String> {
        self.stop_subscription_ticker();
        {
            let mut guard = self
                .session
                .lock()
                .map_err(|e| format!("session lock poisoned: {}", e))?;
            *guard = None;
        }
        self.user_paths.clear_user();
        self.runtime.set_session_activated(false, 0).await;
        log::info!("[AppRuntime] Session deactivated");
        Ok(())
    }

    /// Whether a per-user session is currently active.
    pub fn is_session_active(&self) -> bool {
        self.session
            .lock()
            .map(|guard| guard.is_some())
            .unwrap_or(false)
    }

    /// The active user id, if a session is active.
    pub fn active_user_id(&self) -> Option<u64> {
        self.session
            .lock()
            .ok()
            .and_then(|guard| guard.as_ref().map(|s| s.user_id))
    }

    /// Run a closure with the active session store.
    ///
    /// Returns `None` when no session is active. This hands the shell the
    /// real [`SessionStore`] API without duplicating its methods on the
    /// facade.
    pub fn with_session_store<F, R>(&self, f: F) -> Option<R>
    where
        F: FnOnce(&SessionStore) -> R,
    {
        let guard = self.session.lock().ok()?;
        guard.as_ref().map(|s| f(&s.session_store))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::RuntimeState;
    use crate::session_store::PersistedSessionSnapshot;
    use qbz_core::NoOpAdapter;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn unique_test_dir(name: &str) -> std::path::PathBuf {
        static NONCE: AtomicU64 = AtomicU64::new(0);
        let nonce = NONCE.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("qbz-app-{name}-{}-{nonce}", std::process::id()))
    }

    fn test_runtime() -> AppRuntime<NoOpAdapter> {
        // QbzCore builds a reqwest client (MusicBrainz); since reqwest no
        // longer pulls a rustls provider (#663), tests must install the
        // process-level one explicitly, exactly like the real binaries do at
        // startup — otherwise Client construction panics with "No provider set".
        crate::ensure_crypto_provider();
        AppRuntime::with_audio_settings(NoOpAdapter, None, AudioSettings::default(), None)
    }

    #[test]
    fn builds_with_explicit_audio_settings() {
        let rt = test_runtime();
        let _core = rt.core();
        assert!(!rt.is_session_active());
        assert_eq!(rt.active_user_id(), None);
    }

    #[tokio::test]
    async fn runtime_state_machine_starts_uninitialized() {
        let rt = test_runtime();
        assert_eq!(
            rt.runtime().get_status().await.state,
            RuntimeState::Uninitialized
        );
    }

    #[tokio::test]
    async fn core_reports_no_session_before_login() {
        let rt = test_runtime();
        assert!(!rt.core().has_session().await);
        assert!(!rt.core().is_api_initialized().await);
    }

    #[tokio::test]
    async fn activate_at_opens_session_and_marks_runtime() {
        let rt = test_runtime();
        let data_dir = unique_test_dir("activate-data");
        let cache_dir = unique_test_dir("activate-cache");

        rt.activate_at(42, &data_dir, &cache_dir)
            .await
            .expect("activation succeeds");

        assert!(rt.is_session_active());
        assert_eq!(rt.active_user_id(), Some(42));
        assert!(rt.runtime().get_status().await.session_activated);
        assert!(data_dir.join("session.db").exists());
        assert!(cache_dir.exists());

        let _ = std::fs::remove_dir_all(&data_dir);
        let _ = std::fs::remove_dir_all(&cache_dir);
    }

    /// User zero is the first-run, never-authenticated profile selected by
    /// `activate_offline`. It is a real local session, not an auth sentinel.
    #[tokio::test]
    async fn guest_profile_zero_is_a_valid_local_session() {
        let rt = test_runtime();
        let data_dir = unique_test_dir("guest-data");
        let cache_dir = unique_test_dir("guest-cache");

        rt.activate_at(0, &data_dir, &cache_dir)
            .await
            .expect("guest activation succeeds without Qobuz auth");

        assert!(rt.is_session_active());
        assert_eq!(rt.active_user_id(), Some(0));
        assert!(data_dir.join("session.db").exists());

        rt.deactivate().await.expect("guest deactivation succeeds");
        let _ = std::fs::remove_dir_all(&data_dir);
        let _ = std::fs::remove_dir_all(&cache_dir);
    }

    #[tokio::test]
    async fn deactivate_clears_session_and_runtime() {
        let rt = test_runtime();
        let data_dir = unique_test_dir("deactivate-data");
        let cache_dir = unique_test_dir("deactivate-cache");

        rt.activate_at(7, &data_dir, &cache_dir)
            .await
            .expect("activation succeeds");
        rt.deactivate().await.expect("deactivation succeeds");

        assert!(!rt.is_session_active());
        assert_eq!(rt.active_user_id(), None);
        assert!(!rt.runtime().get_status().await.session_activated);

        let _ = std::fs::remove_dir_all(&data_dir);
        let _ = std::fs::remove_dir_all(&cache_dir);
    }

    #[tokio::test]
    async fn with_session_store_round_trips_through_active_session() {
        let rt = test_runtime();
        let data_dir = unique_test_dir("store-data");
        let cache_dir = unique_test_dir("store-cache");

        // No session yet: closure is not run.
        assert!(rt.with_session_store(|_| ()).is_none());

        rt.activate_at(1, &data_dir, &cache_dir)
            .await
            .expect("activation succeeds");

        let snapshot = PersistedSessionSnapshot::default();
        rt.with_session_store(|store| store.save_session(&snapshot))
            .expect("session is active")
            .expect("save succeeds");

        let loaded = rt
            .with_session_store(|store| store.load_session())
            .expect("session is active")
            .expect("load succeeds");
        assert_eq!(
            loaded.playback.queue_tracks.len(),
            snapshot.playback.queue_tracks.len()
        );

        let _ = std::fs::remove_dir_all(&data_dir);
        let _ = std::fs::remove_dir_all(&cache_dir);
    }
}
