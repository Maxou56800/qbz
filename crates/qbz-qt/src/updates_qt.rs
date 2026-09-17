//! Local application updates. Orbit's selected host never owns this state.
use cxx_qt_lib::QString;
use qbz_updater::{Asset, Release, install::Installation, store::Store};
use serde::Serialize;
use std::sync::{
    LazyLock, Mutex,
    atomic::{AtomicBool, Ordering},
};

static STARTED: AtomicBool = AtomicBool::new(false);
static CANCEL: AtomicBool = AtomicBool::new(false);
static STATE: LazyLock<Mutex<State>> = LazyLock::new(|| Mutex::new(State::default()));

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct State {
    open: bool,
    phase: String,
    busy: bool,
    check_on_launch: bool,
    current_version: &'static str,
    version: String,
    release_url: String,
    install_method: &'static str,
    can_install: bool,
    downloaded: u64,
    total: Option<u64>,
    error: String,
    percent: Option<u32>,
    #[serde(skip)]
    reminder_key: String,
    #[cfg(target_os = "linux")]
    #[serde(skip)]
    flatpak: Option<std::sync::Arc<qbz_updater::flatpak::Monitor>>,
    #[serde(skip)]
    manual: bool,
    #[serde(skip)]
    release: Option<Release>,
    #[serde(skip)]
    asset: Option<Asset>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            open: false,
            phase: "idle".into(),
            busy: false,
            check_on_launch: true,
            current_version: crate::about_qt::app_version(),
            version: String::new(),
            release_url: qbz_updater::RELEASE_PAGE.into(),
            install_method: Installation::detect().label(),
            can_install: false,
            downloaded: 0,
            total: None,
            error: String::new(),
            percent: None,
            reminder_key: String::new(),
            #[cfg(target_os = "linux")]
            flatpak: None,
            manual: false,
            release: None,
            asset: None,
        }
    }
}

fn update(f: impl FnOnce(&mut State)) {
    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    f(&mut state);
    let json = serde_json::to_string(&*state).unwrap_or_else(|_| "{}".into());
    // Enqueue while holding the state lock, so publishes preserve mutation order.
    crate::about_bridge::ui(move |mut b| b.as_mut().set_updates_json(QString::from(&json)));
}
fn store() -> Result<Store, String> {
    let path = dirs::data_dir()
        .ok_or("No application data directory")?
        .join("qbz/updates.db");
    Store::open(&path)
}
fn failed(error: String) {
    log::warn!("[updates] {error}");
    update(|s| {
        s.busy = false;
        s.phase = "error".into();
        s.error = error;
    });
}

fn preference_failed(error: String) {
    log::warn!("[updates] {error}");
    update(|s| {
        // A settings write must never unlock an active installer/check.
        if !s.busy {
            s.phase = "error".into();
            s.error = error;
            s.open = true;
        }
    });
}

/// Called after the shell is mounted, once per process. No polling timer.
pub fn launch() {
    if STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    crate::spawn(async {
        let pref = tokio::task::spawn_blocking(|| store()?.check_on_launch()).await;
        match pref {
            Ok(Ok(enabled)) => {
                update(|s| s.check_on_launch = enabled);
                if enabled {
                    begin_check(false);
                }
            }
            result => log::warn!("[updates] startup preference unavailable: {result:?}"),
        }
    });
}
pub fn check() {
    begin_check(true);
}
fn begin_check(manual: bool) {
    let mut start = false;
    update(|s| {
        if manual {
            s.open = true;
            s.manual = true;
        }
        if !s.busy && s.phase != "installed" && s.phase != "prepared" {
            s.busy = true;
            s.phase = "checking".into();
            s.error.clear();
            s.manual = manual;
            s.asset = None;
            s.release = None;
            s.can_install = false;
            s.version.clear();
            s.reminder_key.clear();
            s.percent = None;
            #[cfg(target_os = "linux")]
            {
                s.flatpak = None;
            }
            start = true;
        }
    });
    if !start {
        return;
    }
    crate::spawn(async {
        #[cfg(target_os = "linux")]
        if Installation::detect() == Installation::Flatpak {
            check_flatpak().await;
            return;
        }
        // Fetch without suppression first: a manual request can join a launch
        // request while it is in flight and must still see the actual latest.
        match qbz_updater::check(crate::about_qt::app_version(), false).await {
            Err(error) => failed(error),
            Ok(None) => update(|s| {
                s.busy = false;
                s.phase = "current".into();
            }),
            Ok(Some(release)) => {
                let version = match release.version() {
                    Ok(v) => v.to_string(),
                    Err(e) => {
                        failed(e);
                        return;
                    }
                };
                let suppression_version = version.clone();
                let suppressed =
                    tokio::task::spawn_blocking(move || store()?.suppressed(&suppression_version))
                        .await;
                let suppressed = !matches!(suppressed, Ok(Ok(false)));
                let old_enough = release.notification_ready();
                let asset = if let Some(platform) = Installation::detect().platform() {
                    match qbz_updater::asset(&release, &platform).await {
                        Ok(asset) => Some(asset),
                        Err(e) => {
                            log::info!("[updates] signed installer not available yet: {e}");
                            None
                        }
                    }
                } else {
                    None
                };
                update(|s| {
                    s.busy = false;
                    s.phase = "available".into();
                    s.version = version;
                    s.reminder_key = s.version.clone();
                    s.release_url = release.page();
                    s.release = Some(release);
                    s.can_install = asset.is_some();
                    s.asset = asset;
                    if !s.manual && s.check_on_launch && old_enough && !suppressed {
                        s.open = true;
                    }
                });
            }
        }
    });
}
pub fn close() {
    update(|s| s.open = false);
}
pub fn set_launch(enabled: bool) {
    crate::spawn(async move {
        match tokio::task::spawn_blocking(move || store()?.set_check_on_launch(enabled)).await {
            Ok(Ok(())) => update(|s| s.check_on_launch = enabled),
            error => preference_failed(format!("Could not save update preferences: {error:?}")),
        }
    });
}
pub fn ignore() {
    let version = STATE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .reminder_key
        .clone();
    if version.is_empty() {
        return;
    }
    crate::spawn(async move {
        match tokio::task::spawn_blocking(move || store()?.ignore(&version)).await {
            Ok(Ok(())) => close(),
            error => preference_failed(format!("Could not save release reminder: {error:?}")),
        }
    });
}
pub fn cancel() {
    CANCEL.store(true, Ordering::Relaxed);
}
pub fn install() {
    let mut asset = None;
    #[cfg(target_os = "linux")]
    let mut flatpak = None;
    update(|s| {
        if !s.busy && s.can_install && s.phase != "installed" {
            #[cfg(target_os = "linux")]
            if let Some(monitor) = &s.flatpak {
                flatpak = Some(monitor.clone());
                s.busy = true;
                s.open = true;
                s.phase = "updating".into();
                s.error.clear();
                s.percent = Some(0);
                CANCEL.store(false, Ordering::Relaxed);
                return;
            }
            asset = s.asset.clone();
            if asset.is_some() {
                s.busy = true;
                s.open = true;
                s.phase = "downloading".into();
                s.downloaded = 0;
                s.total = None;
                s.error.clear();
                CANCEL.store(false, Ordering::Relaxed);
            }
        }
    });
    #[cfg(target_os = "linux")]
    if let Some(monitor) = flatpak {
        crate::spawn(async move {
            let result = monitor
                .install(&CANCEL, |percent| update(|s| s.percent = Some(percent)))
                .await;
            update(|s| {
                s.flatpak = None;
                s.can_install = false;
            });
            match result {
                Ok(()) => update(|s| {
                    s.phase = "installed".into();
                    s.busy = false;
                }),
                Err(_) if CANCEL.load(Ordering::Relaxed) => update(|s| {
                    s.phase = "cancelled".into();
                    s.busy = false;
                }),
                Err(error) => failed(error),
            }
        });
        return;
    }
    let Some(asset) = asset else { return };
    crate::spawn(async move {
        let result = qbz_updater::install::install(
            Installation::detect(),
            asset,
            &CANCEL,
            |phase, downloaded, total| {
                update(|s| {
                    s.phase = phase.into();
                    s.downloaded = downloaded;
                    s.total = total;
                })
            },
        )
        .await;
        match result {
            Ok(outcome) => {
                let (staged, backup) = match outcome {
                    qbz_updater::install::InstallOutcome::Installed(path) => (false, path),
                    qbz_updater::install::InstallOutcome::Prepared(path) => (true, path),
                };
                log::info!(
                    "[updates] completed staging/install; recovery or installer files: {}",
                    backup.display()
                );
                update(|s| {
                    s.phase = if staged { "prepared" } else { "installed" }.into();
                    s.busy = false;
                    s.can_install = false;
                });
            }
            Err(_) if CANCEL.load(Ordering::Relaxed) => update(|s| {
                s.phase = "cancelled".into();
                s.busy = false;
            }),
            Err(error) => failed(error),
        }
    });
}

#[cfg(target_os = "linux")]
async fn check_flatpak() {
    use qbz_updater::flatpak::{Availability, Monitor};
    match Monitor::check().await {
        Err(error) => failed(error),
        Ok(monitor) => {
            let key = monitor.info.reminder_key();
            let suppression_key = key.clone();
            let suppressed =
                tokio::task::spawn_blocking(move || store()?.suppressed(&suppression_key)).await;
            let suppressed = !matches!(suppressed, Ok(Ok(false)));
            update(|s| {
                s.busy = false;
                s.reminder_key = key;
                match monitor.info.availability() {
                    Availability::Current => s.phase = "current".into(),
                    Availability::Restart => s.phase = "installed".into(),
                    Availability::Available => {
                        s.phase = "available".into();
                        s.can_install = true;
                        s.flatpak = Some(std::sync::Arc::new(monitor));
                        if !s.manual && s.check_on_launch && !suppressed {
                            s.open = true;
                        }
                    }
                }
            });
        }
    }
}
