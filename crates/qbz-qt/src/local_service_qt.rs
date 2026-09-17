//! Desktop binding of the shared host-owned library service. Workers retain the
//! captured binding; only this boundary selects desktop profile defaults.
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use qbz_library::service::LibraryService;
use qbz_library::LibraryStore;

pub struct DesktopLibrary {
    pub service: Arc<LibraryService>,
    observing: AtomicBool,
}

impl DesktopLibrary {
    pub fn begin_observing(&self) -> bool {
        !self.observing.swap(true, Ordering::AcqRel)
    }
    pub fn stop_observing(&self) {
        self.observing.store(false, Ordering::Release);
    }
}

static HOST: Mutex<Option<Arc<DesktopLibrary>>> = Mutex::new(None);
// Accessed while holding HOST. Once a transition starts, background observers
// cannot create a new guest binding between quiesce and explicit activation.
static ALLOW_LAZY_BIND: AtomicBool = AtomicBool::new(true);

fn create(dir: &Path) -> Arc<DesktopLibrary> {
    Arc::new(DesktopLibrary {
        service: Arc::new(LibraryService::new(
            LibraryStore::new(dir.join("library.db")),
            // Retain the desktop's stable artwork cache across guest-profile
            // adoption: moving users/0 must not invalidate indexed absolute
            // cover paths. Other hosts supply their own cache to the service.
            dirs::cache_dir()
                .map(|p| p.join("qbz/artwork"))
                .unwrap_or_else(|| {
                    dir.parent()
                        .and_then(Path::parent)
                        .unwrap_or(dir)
                        .join("artwork")
                }),
        )),
        observing: AtomicBool::new(false),
    })
}

pub fn current() -> Option<Arc<DesktopLibrary>> {
    let mut host = HOST.lock().unwrap_or_else(|e| e.into_inner());
    if host.is_none() {
        if !ALLOW_LAZY_BIND.load(Ordering::Relaxed) {
            return None;
        }
        let uid = qbz_app::user_data::UserDataPaths::load_last_user_id().unwrap_or(0);
        let dir = dirs::data_dir()?.join("qbz/users").join(uid.to_string());
        *host = Some(create(&dir));
    }
    host.clone()
}

pub fn is_current(host: &Arc<DesktopLibrary>) -> bool {
    HOST.lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .is_some_and(|current| Arc::ptr_eq(host, current) && !current.service.snapshot().closed)
}

pub fn bind(dir: &Path) {
    let old = {
        let mut host = HOST.lock().unwrap_or_else(|e| e.into_inner());
        ALLOW_LAZY_BIND.store(true, Ordering::Relaxed);
        if host.as_ref().is_some_and(|h| {
            !h.service.snapshot().closed
                && h.service.store().database_path() == dir.join("library.db")
        }) {
            return;
        }
        host.replace(create(dir))
    };
    crate::orbit_qt::reset();
    retire(old);
    crate::settings_qt::library::reset_profile_state();
}

pub fn reset() {
    // Retain the closed binding until activation: a following login must
    // still be able to join its worker before adopting/renaming guest data.
    let old = {
        let host = HOST.lock().unwrap_or_else(|e| e.into_inner());
        ALLOW_LAZY_BIND.store(false, Ordering::Relaxed);
        host.clone()
    };
    crate::orbit_qt::reset();
    retire(old);
    crate::settings_qt::library::reset_profile_state();
}

fn retire(old: Option<Arc<DesktopLibrary>>) {
    if let Some(old) = old {
        old.service.close();
        // Never wait for a network mount on the UI thread. The old worker can
        // only finish against its captured paths; it cannot publish to new UI.
        crate::spawn(async move {
            let _ = tokio::task::spawn_blocking(move || old.service.shutdown()).await;
        });
    }
}

/// Join before AppRuntime can rename a guest profile. This wait runs on the
/// blocking pool; a slow filesystem must not turn into a scan writing through
/// a directory while that directory is being adopted by another account.
pub async fn quiesce() {
    let host = {
        let host = HOST.lock().unwrap_or_else(|e| e.into_inner());
        ALLOW_LAZY_BIND.store(false, Ordering::Relaxed);
        host.clone()
    };
    if let Some(host) = host {
        host.service.close();
        crate::orbit_qt::reset();
        let _ = tokio::task::spawn_blocking(move || host.service.shutdown()).await;
    }
}
