//! Background maintenance hints for the incremental local scanner (phase G).
//!
//! Native filesystem events debounce into a root-scoped scan for local disks.
//! They are never the source of truth: every root also receives a periodic
//! reconciliation, and network roots use only that bounded schedule.

use std::collections::BTreeMap;
use std::sync::Once;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use qbz_library::{LibraryFolder, LocalRootWatcher, RootWatchEvent};

const WATCH_POLL: Duration = Duration::from_secs(1);
const WATCH_DEBOUNCE: Duration = Duration::from_secs(2);
const ROOT_REFRESH: Duration = Duration::from_secs(60);
const LOCAL_RECONCILE: Duration = Duration::from_secs(24 * 60 * 60);
const NETWORK_RECONCILE: Duration = Duration::from_secs(6 * 60 * 60);
const BUSY_RETRY: Duration = Duration::from_secs(15);
// Failed/unavailable roots do not advance last_scan. Do not turn the one-minute
// folder refresh into a scan loop for those roots; leave the stored success
// timestamp intact and bound retries within this watcher session.
const RECONCILE_RETRY: Duration = Duration::from_secs(5 * 60);

pub(crate) fn start() {
    static START: Once = Once::new();
    START.call_once(|| {
        let _ = std::thread::Builder::new()
            .name("qbz-local-scan-maintenance".to_string())
            .spawn(run);
    });
}

fn run() {
    loop {
        if let Some(host) =
            crate::local_service_qt::current().filter(crate::local_service_qt::is_current)
        {
            // A missing profile is not a reason to create the legacy global
            // library.db. Wait for the first real library write instead.
            match host
                .service
                .store()
                .read(|db| db.get_folders_with_metadata())
            {
                Ok(Some(folders)) => watch_profile(host, folders),
                Ok(None) => {}
                Err(error) => log::warn!("[local-scan] profile library unavailable: {error}"),
            }
        }
        std::thread::sleep(WATCH_POLL);
    }
}

fn watch_profile(
    host: std::sync::Arc<crate::local_service_qt::DesktopLibrary>,
    mut folders: Vec<LibraryFolder>,
) {
    let mut watcher = LocalRootWatcher::new(&folders).ok();
    let mut pending = BTreeMap::<i64, Instant>::new();
    let mut attempted = BTreeMap::<i64, Instant>::new();
    queue_periodic_due(
        &folders,
        unix_now(),
        Instant::now(),
        &attempted,
        &mut pending,
    );
    let mut refreshed = Instant::now();

    while crate::local_service_qt::is_current(&host) {
        let event = watcher
            .as_ref()
            .map(|watcher| watcher.recv_timeout(WATCH_POLL))
            .unwrap_or_else(|| {
                std::thread::sleep(WATCH_POLL);
                RootWatchEvent::Timeout
            });
        if !crate::local_service_qt::is_current(&host) {
            return;
        }
        let now = Instant::now();
        match event {
            RootWatchEvent::Changed(root_ids) => {
                for root_id in root_ids {
                    pending.insert(root_id, now + WATCH_DEBOUNCE);
                }
            }
            RootWatchEvent::Error(root_ids) => {
                // A watcher error is only loss of an accelerator. Queue the
                // affected roots and let the generation scan decide safety.
                for root_id in root_ids {
                    pending.insert(root_id, now + WATCH_DEBOUNCE);
                }
            }
            RootWatchEvent::Disconnected => {
                watcher = None;
            }
            RootWatchEvent::Timeout => {}
        }

        if now.duration_since(refreshed) >= ROOT_REFRESH {
            let Ok(Some(current)) = host
                .service
                .store()
                .read(|db| db.get_folders_with_metadata())
            else {
                return;
            };
            attempted.retain(|id, _| {
                current.iter().any(|new| {
                    new.id == *id
                        && new.enabled
                        && folders
                            .iter()
                            .any(|old| old.id == *id && old.path == new.path && old.enabled)
                })
            });
            pending.retain(|id, _| {
                current
                    .iter()
                    .any(|folder| folder.id == *id && folder.enabled)
            });
            folders = current;
            match watcher.as_mut() {
                Some(active_watcher) => {
                    if active_watcher.rebuild(&folders).is_err() {
                        watcher = None;
                    }
                }
                None => watcher = LocalRootWatcher::new(&folders).ok(),
            }
            queue_periodic_due(&folders, unix_now(), now, &attempted, &mut pending);
            refreshed = now;
        }

        let ready = pending
            .iter()
            .find(|(_, deadline)| **deadline <= now)
            .map(|(root_id, _)| *root_id);
        let Some(root_id) = ready else {
            continue;
        };
        if crate::settings_qt::library::scan_for(host.clone(), Some(root_id), true) {
            attempted.insert(root_id, now);
            pending.remove(&root_id);
        } else {
            pending.insert(root_id, now + BUSY_RETRY);
        }
    }
}

fn queue_periodic_due(
    folders: &[LibraryFolder],
    now_secs: i64,
    now: Instant,
    attempted: &BTreeMap<i64, Instant>,
    pending: &mut BTreeMap<i64, Instant>,
) {
    for folder in folders.iter().filter(|folder| folder.enabled) {
        if reconciliation_due(folder, now_secs)
            && attempted
                .get(&folder.id)
                .is_none_or(|last| now.duration_since(*last) >= RECONCILE_RETRY)
        {
            pending.entry(folder.id).or_insert(now);
        }
    }
}

fn reconciliation_due(folder: &LibraryFolder, now_secs: i64) -> bool {
    let interval = if folder.is_network {
        NETWORK_RECONCILE
    } else {
        LOCAL_RECONCILE
    };
    let last = folder.last_scan.unwrap_or(0);
    last <= 0 || now_secs.saturating_sub(last) >= interval.as_secs().min(i64::MAX as u64) as i64
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs().min(i64::MAX as u64) as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(network: bool, last_scan: Option<i64>) -> LibraryFolder {
        LibraryFolder {
            id: 1,
            path: "/fixture/music".to_string(),
            alias: None,
            enabled: true,
            is_network: network,
            network_fs_type: network.then(|| "nfs".to_string()),
            user_override_network: network,
            last_scan,
        }
    }

    #[test]
    fn local_watch_roots_still_reconcile_daily() {
        let now = 2_000_000;
        assert!(reconciliation_due(&folder(false, None), now));
        assert!(!reconciliation_due(
            &folder(false, Some(now - LOCAL_RECONCILE.as_secs() as i64 + 1)),
            now
        ));
        assert!(reconciliation_due(
            &folder(false, Some(now - LOCAL_RECONCILE.as_secs() as i64)),
            now
        ));
    }

    #[test]
    fn network_roots_use_the_shorter_periodic_schedule() {
        let now = 2_000_000;
        assert!(reconciliation_due(
            &folder(true, Some(now - NETWORK_RECONCILE.as_secs() as i64)),
            now
        ));
        assert!(!reconciliation_due(
            &folder(true, Some(now - NETWORK_RECONCILE.as_secs() as i64 + 1)),
            now
        ));
    }

    #[test]
    fn unavailable_root_does_not_rescan_on_every_folder_refresh() {
        let now = Instant::now();
        let folders = [folder(false, None)];
        let mut pending = BTreeMap::new();
        let mut attempted = BTreeMap::new();
        queue_periodic_due(&folders, 2_000_000, now, &attempted, &mut pending);
        assert!(pending.remove(&1).is_some());
        attempted.insert(1, now);
        for minute in 1..5 {
            queue_periodic_due(
                &folders,
                2_000_000 + minute * 60,
                now + Duration::from_secs(minute as u64 * 60),
                &attempted,
                &mut pending,
            );
            assert!(pending.is_empty());
        }
        queue_periodic_due(
            &folders,
            2_000_300,
            now + RECONCILE_RETRY,
            &attempted,
            &mut pending,
        );
        assert!(pending.contains_key(&1));
    }
}
