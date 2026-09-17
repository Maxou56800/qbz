//! Best-effort local-root watcher.
//!
//! Watch notifications are only acceleration hints. The scheduler always
//! performs periodic generation scans as the source of truth, and network
//! roots are deliberately excluded because recursive watches on NAS mounts are
//! not reliable enough to authorize deletion.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::Duration;

use notify::event::{AccessKind, AccessMode, MetadataKind, ModifyKind};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};

use crate::{LibraryError, LibraryFolder};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootWatchEvent {
    Changed(Vec<i64>),
    Error(Vec<i64>),
    Disconnected,
    Timeout,
}

pub struct LocalRootWatcher {
    watcher: RecommendedWatcher,
    receiver: Receiver<notify::Result<Event>>,
    roots: BTreeMap<i64, PathBuf>,
    failures: BTreeMap<i64, (PathBuf, String)>,
}

impl LocalRootWatcher {
    pub fn new(folders: &[LibraryFolder]) -> Result<Self, LibraryError> {
        let (sender, receiver) = mpsc::channel();
        let watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
            // Filter before enqueueing: our own directory walks and playback
            // reads otherwise schedule another scan indefinitely.
            if event.as_ref().map_or(true, changes_library) {
                let _ = sender.send(event);
            }
        })
        .map_err(|error| LibraryError::Other(format!("local root watcher: {error}")))?;
        let mut result = Self {
            watcher,
            receiver,
            roots: BTreeMap::new(),
            failures: BTreeMap::new(),
        };
        result.rebuild(folders)?;
        Ok(result)
    }

    pub fn rebuild(&mut self, folders: &[LibraryFolder]) -> Result<(), LibraryError> {
        let desired = folders
            .iter()
            .filter(|folder| folder.enabled && !folder.is_network)
            .map(|folder| (folder.id, PathBuf::from(&folder.path)))
            .collect::<BTreeMap<_, _>>();

        for (root_id, path) in self.roots.clone() {
            if desired.get(&root_id) == Some(&path) {
                continue;
            }
            let _ = self.watcher.unwatch(&path);
            self.roots.remove(&root_id);
        }
        self.failures.retain(|id, (path, _)| desired.get(id) == Some(path));
        for (root_id, path) in &desired {
            if self.roots.get(root_id) == Some(path) {
                continue;
            }
            // A missing/newly-unmounted root remains covered by periodic
            // reconciliation. Failing to install its hint is not fatal.
            match self.watcher.watch(path, RecursiveMode::Recursive) {
                Ok(()) => {
                    self.roots.insert(*root_id, path.clone());
                    if self.failures.remove(root_id).is_some() {
                        log::info!(
                            "[local-scan] watcher restored root_id={root_id} path={}",
                            path.display()
                        );
                    }
                }
                Err(error) => {
                    let error = error.to_string();
                    if self.record_failure(*root_id, path.clone(), error.clone()) {
                        log::warn!(
                            "[local-scan] watcher unavailable root_id={root_id} path={} error={error}; periodic reconciliation remains active",
                            path.display()
                        );
                    }
                }
            }
        }
        Ok(())
    }

    // Report a failure once per path/cause, then again after recovery. Keep
    // retrying registration so removable folders recover without a restart.
    fn record_failure(&mut self, id: i64, path: PathBuf, error: String) -> bool {
        let failure = (path, error);
        if self.failures.get(&id) == Some(&failure) {
            return false;
        }
        self.failures.insert(id, failure);
        true
    }

    pub fn recv_timeout(&self, timeout: Duration) -> RootWatchEvent {
        match self.receiver.recv_timeout(timeout) {
            Ok(Ok(event)) if event.need_rescan() => {
                RootWatchEvent::Changed(self.roots.keys().copied().collect())
            }
            Ok(Ok(event)) => RootWatchEvent::Changed(self.roots_for_paths(&event.paths)),
            Ok(Err(error)) if error.paths.is_empty() => {
                RootWatchEvent::Error(self.roots.keys().copied().collect())
            }
            Ok(Err(error)) => RootWatchEvent::Error(self.roots_for_paths(&error.paths)),
            Err(RecvTimeoutError::Timeout) => RootWatchEvent::Timeout,
            Err(RecvTimeoutError::Disconnected) => RootWatchEvent::Disconnected,
        }
    }

    fn roots_for_paths(&self, paths: &[PathBuf]) -> Vec<i64> {
        let mut roots = BTreeSet::new();
        for path in paths {
            for (root_id, root) in &self.roots {
                if path.starts_with(root) {
                    roots.insert(*root_id);
                }
            }
        }
        roots.into_iter().collect()
    }

    #[cfg(test)]
    fn watched_roots(&self) -> usize {
        self.roots.len()
    }
}

fn changes_library(event: &Event) -> bool {
    if event.need_rescan() {
        return true;
    }
    match event.kind {
        // Preserve the completed-write hint: a preceding Modify can arrive
        // while an external writer still has an incomplete audio file open.
        EventKind::Access(AccessKind::Close(AccessMode::Write)) => true,
        EventKind::Access(_) => false,
        EventKind::Modify(ModifyKind::Metadata(MetadataKind::AccessTime)) => false,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_do_not_schedule_scans_but_content_changes_and_overflow_do() {
        use notify::event::{CreateKind, DataChange, Flag, RemoveKind, RenameMode};
        for kind in [
            EventKind::Access(AccessKind::Read),
            EventKind::Access(AccessKind::Open(AccessMode::Any)),
            EventKind::Access(AccessKind::Close(AccessMode::Read)),
            EventKind::Modify(ModifyKind::Metadata(MetadataKind::AccessTime)),
        ] {
            assert!(!changes_library(&Event::new(kind)), "{kind:?}");
            assert!(changes_library(&Event::new(kind).set_flag(Flag::Rescan)));
        }
        for kind in [
            EventKind::Create(CreateKind::File),
            EventKind::Remove(RemoveKind::Folder),
            EventKind::Modify(ModifyKind::Data(DataChange::Any)),
            EventKind::Modify(ModifyKind::Name(RenameMode::Both)),
            EventKind::Modify(ModifyKind::Metadata(MetadataKind::Permissions)),
            EventKind::Access(AccessKind::Close(AccessMode::Write)),
        ] {
            assert!(changes_library(&Event::new(kind)), "{kind:?}");
        }
    }

    #[test]
    fn directory_reads_do_not_feed_back_into_native_watcher() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("track.flac");
        std::fs::write(&path, b"fixture").unwrap();
        let watcher = LocalRootWatcher::new(&[folder(4, temp.path(), false)]).unwrap();
        for _ in 0..3 {
            let _ = std::fs::read_dir(temp.path()).unwrap().collect::<Vec<_>>();
            assert_eq!(std::fs::read(&path).unwrap(), b"fixture");
        }
        assert_eq!(watcher.recv_timeout(Duration::from_millis(300)), RootWatchEvent::Timeout);
        std::fs::write(&path, b"changed").unwrap();
        assert_eq!(watcher.recv_timeout(Duration::from_secs(5)), RootWatchEvent::Changed(vec![4]));
    }

    fn folder(id: i64, path: &std::path::Path, network: bool) -> LibraryFolder {
        LibraryFolder {
            id,
            path: path.to_string_lossy().into_owned(),
            alias: None,
            enabled: true,
            is_network: network,
            network_fs_type: network.then(|| "nfs".to_string()),
            user_override_network: network,
            last_scan: None,
        }
    }

    #[test]
    fn missing_root_retries_recovers_and_forgets_removed_failures() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("removable");
        let folders = [folder(7, &path, false)];
        let mut watcher = LocalRootWatcher::new(&folders).unwrap();
        assert_eq!(watcher.watched_roots(), 0);
        assert_eq!(watcher.failures.len(), 1);
        let failure = watcher.failures[&7].clone();
        watcher.rebuild(&folders).unwrap();
        assert_eq!(watcher.failures[&7], failure);
        assert!(!watcher.record_failure(7, failure.0, failure.1));
        assert!(watcher.record_failure(7, path.clone(), "changed cause".into()));
        std::fs::create_dir(&path).unwrap();
        watcher.rebuild(&folders).unwrap();
        assert_eq!(watcher.watched_roots(), 1);
        assert!(watcher.failures.is_empty());
        watcher.rebuild(&[]).unwrap();
        std::fs::remove_dir(&path).unwrap();
        watcher.rebuild(&folders).unwrap();
        assert_eq!(watcher.failures.len(), 1);
        watcher.rebuild(&[]).unwrap();
        assert!(watcher.failures.is_empty());
    }

    #[test]
    fn only_local_roots_are_watched_and_nested_path_queues_every_owner() {
        let temp = tempfile::tempdir().unwrap();
        let local = temp.path().join("local");
        let nested = local.join("nested");
        let network = temp.path().join("network");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(&network).unwrap();
        let watcher = LocalRootWatcher::new(&[
            folder(1, &local, false),
            folder(2, &nested, false),
            folder(3, &network, true),
        ])
        .unwrap();
        assert_eq!(watcher.watched_roots(), 2);
        assert_eq!(
            watcher.roots_for_paths(&[nested.join("track.flac")]),
            vec![1, 2]
        );
    }
}
