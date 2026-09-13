//! Host-owned library work. No desktop globals, runtime or Qobuz session.
//!
//! A scan captures the database and artwork roots for the lifetime of this
//! service. Requests coalesce behind one worker; cancellation clears the queue.
//! Hosts poll progress/revision to invalidate their own derived views. Shutdown
//! is cooperative: call `close` immediately, then `shutdown` on a blocking thread
//! to join (filesystem calls on an unavailable mount may take time to return).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use crate::{LibraryError, LibraryStore, ScanEvent, ScanStatus};

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct ScanProgress {
    pub running: bool,
    pub job: u64,
    pub processed: u32,
    pub total: u32,
    pub source_processed: u32,
    pub source_total: u32,
    pub current_root_id: i64,
    pub source_index: u32,
    pub source_count: u32,
    /// Basename only; full host paths are not needed by a progress indicator.
    pub file: String,
    pub cleaning: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanOutcome {
    Complete,
    Cancelled,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ScanCompletion {
    pub job: u64,
    pub background: bool,
    pub outcome: ScanOutcome,
    pub skipped: usize,
}

#[derive(Clone, Debug, Default, serde::Serialize)]
pub struct LibrarySnapshot {
    pub progress: ScanProgress,
    pub last_scan: Option<ScanCompletion>,
    /// Keep a manual result available even if background work finishes between UI polls.
    #[serde(skip_serializing)]
    pub last_requested_scan: Option<ScanCompletion>,
    /// Changes after each scan, even a partial/cancelled one: committed batches
    /// remain authoritative. This is an invalidation signal, not a DB cursor.
    pub revision: u64,
    /// Changes only when scans write catalog rows (or a folder is registered).
    /// Completion/progress still advances for an unchanged or unavailable root.
    pub content_revision: u64,
    pub queued: usize,
    pub closed: bool,
}

#[derive(Default)]
struct PendingScans {
    all: Option<bool>,
    folders: BTreeMap<i64, bool>,
}

impl PendingScans {
    fn push(&mut self, id: Option<i64>) {
        self.push_with_background(id, false);
    }

    fn push_with_background(&mut self, id: Option<i64>, background: bool) {
        match id {
            None => {
                self.all = Some(
                    background
                        && self.all.unwrap_or(true)
                        && self.folders.values().all(|background| *background),
                );
                self.folders.clear();
            }
            Some(id) if self.all.is_none() => {
                self.folders
                    .entry(id)
                    .and_modify(|old| *old &= background)
                    .or_insert(background);
            }
            Some(_) => *self.all.as_mut().unwrap() &= background,
        }
    }

    fn pop(&mut self) -> Option<(Option<i64>, bool)> {
        if let Some(background) = self.all.take() {
            Some((None, background))
        } else {
            self.folders
                .pop_first()
                .map(|(id, background)| (Some(id), background))
        }
    }

    fn len(&self) -> usize {
        usize::from(self.all.is_some()) + self.folders.len()
    }
    fn clear(&mut self) {
        *self = Self::default();
    }
}

#[derive(Default)]
struct State {
    snapshot: LibrarySnapshot,
    pending: PendingScans,
    source_base: u32,
    background: bool,
}

impl State {
    fn record_completion(&mut self, completion: ScanCompletion) {
        if !completion.background {
            self.snapshot.last_requested_scan = Some(completion.clone());
        }
        self.snapshot.last_scan = Some(completion);
    }
}

struct Shared {
    store: LibraryStore,
    artwork: PathBuf,
    state: Mutex<State>,
    idle: Condvar,
    cancel: AtomicBool,
    #[cfg(test)]
    observer: Mutex<Option<Box<dyn Fn(&ScanEvent) + Send + Sync>>>,
}

/// Administration failures are distinct from an empty catalog or accepted job.
#[derive(Debug, thiserror::Error)]
pub enum FolderError {
    #[error("library host is closed")]
    Closed,
    #[error("library scan is running")]
    Busy,
    #[error("library revision changed")]
    RevisionChanged,
    #[error("expected an existing absolute directory on the host")]
    InvalidPath,
    #[error("folder overlaps an existing library root")]
    Conflict,
    #[error("folder is disabled")]
    Disabled,
    #[error(transparent)]
    Storage(#[from] LibraryError),
}

pub struct LibraryService {
    shared: Arc<Shared>,
    // Serializes worker handoff with requests and shutdown. Never taken by the
    // worker or while publishing progress; joining cannot deadlock on it.
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl LibraryService {
    /// Construction is inert: no directories, database or worker are created.
    pub fn new(store: LibraryStore, artwork: PathBuf) -> Self {
        Self {
            shared: Arc::new(Shared {
                store,
                artwork,
                state: Mutex::new(State::default()),
                idle: Condvar::new(),
                cancel: AtomicBool::new(false),
                #[cfg(test)]
                observer: Mutex::new(None),
            }),
            worker: Mutex::new(None),
        }
    }

    pub fn store(&self) -> &LibraryStore {
        &self.shared.store
    }
    pub fn artwork_directory(&self) -> &Path {
        &self.shared.artwork
    }

    pub fn snapshot(&self) -> LibrarySnapshot {
        let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut snapshot = state.snapshot.clone();
        snapshot.queued = state.pending.len();
        snapshot
    }

    /// Returns false after close. The queue and running flag change under the
    /// same lock, so a request arriving at worker completion cannot be lost.
    pub fn scan(&self, folder: Option<i64>) -> Result<bool, LibraryError> {
        self.scan_with_background(folder, false)
    }

    /// Maintenance keeps the same progress and invalidation contract, without
    /// presenting its completion as a scan requested by the user.
    pub fn scan_background(&self, folder: Option<i64>) -> Result<bool, LibraryError> {
        self.scan_with_background(folder, true)
    }

    fn scan_with_background(
        &self,
        folder: Option<i64>,
        background: bool,
    ) -> Result<bool, LibraryError> {
        let mut worker = self.worker.lock().unwrap_or_else(|e| e.into_inner());
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.snapshot.closed {
            return Ok(false);
        }
        if background {
            state.pending.push_with_background(folder, true);
        } else {
            state.pending.push(folder);
        }
        if state.snapshot.progress.running {
            return Ok(true);
        }
        state.snapshot.progress.running = true;
        drop(state);
        if let Some(previous) = worker.take() {
            let _ = previous.join();
        }
        let shared = self.shared.clone();
        match std::thread::Builder::new()
            .name("library-scan".into())
            .spawn(move || {
                // Always release running, including an unexpected engine panic.
                let result =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(&shared)));
                if result.is_err() {
                    let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
                    let job = state.snapshot.progress.job;
                    let background = state.background;
                    state.record_completion(ScanCompletion {
                        job,
                        background,
                        outcome: ScanOutcome::Failed,
                        skipped: 0,
                    });
                    state.snapshot.revision += 1;
                    // A panic may follow an already committed batch.
                    state.snapshot.content_revision += 1;
                    state.snapshot.progress.running = false;
                    state.pending.clear();
                    shared.idle.notify_all();
                    log::error!("library scan worker panicked");
                }
            }) {
            Ok(handle) => {
                *worker = Some(handle);
                Ok(true)
            }
            Err(error) => {
                let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
                state.snapshot.progress.running = false;
                state.pending.clear();
                self.shared.idle.notify_all();
                Err(error.into())
            }
        }
    }

    /// Register a directory on THIS host, with optimistic revision admission.
    /// The worker handoff lock also makes shutdown join in-flight registration
    /// before a host can move/drop its profile. Never call on the UI thread.
    pub fn register_folder(
        &self,
        path: &Path,
        revision: u64,
    ) -> Result<crate::LibraryFolder, FolderError> {
        let _worker = self.worker.lock().unwrap_or_else(|e| e.into_inner());
        {
            let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
            if state.snapshot.closed {
                return Err(FolderError::Closed);
            }
            if state.snapshot.revision != revision {
                return Err(FolderError::RevisionChanged);
            }
            if state.snapshot.progress.running {
                return Err(FolderError::Busy);
            }
        }
        if !path.is_absolute() {
            return Err(FolderError::InvalidPath);
        }
        let path = path.canonicalize().map_err(|_| FolderError::InvalidPath)?;
        if !path.is_dir() {
            return Err(FolderError::InvalidPath);
        }
        path.to_str().ok_or(FolderError::InvalidPath)?;
        let db = self.shared.store.open_or_create()?;
        use crate::RegisterFolderOutcome;
        let (folder_id, added) = match db.register_or_refresh_folder(&path) {
            RegisterFolderOutcome::Added { folder_id } => (folder_id, true),
            RegisterFolderOutcome::Refreshed { folder_id }
            | RegisterFolderOutcome::Covered { folder_id } => (folder_id, false),
            RegisterFolderOutcome::RegisteredDisabled { .. } => return Err(FolderError::Disabled),
            RegisterFolderOutcome::Conflict => return Err(FolderError::Conflict),
            RegisterFolderOutcome::Failed => {
                return Err(FolderError::Storage(LibraryError::Other(
                    "folder registration failed".into(),
                )))
            }
        };
        let folder = db
            .get_folder_by_id(folder_id)?
            .ok_or_else(|| LibraryError::Other("registered folder missing".into()))?;
        if added {
            let mut state = self.shared
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            state.snapshot.revision += 1;
            state.snapshot.content_revision += 1;
        }
        Ok(folder)
    }

    /// An old Cancel click must never cancel a subsequent scan's job.
    pub fn cancel_job(&self, job: u64) -> bool {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.snapshot.closed
            || !state.snapshot.progress.running
            || state.snapshot.progress.job != job
        {
            return false;
        }
        state.pending.clear();
        self.shared.cancel.store(true, Ordering::Release);
        true
    }

    /// Stop the current scan and discard requests already queued. A NEW request
    /// after this call is accepted, and runs after the cancelled scan unwinds.
    pub fn cancel_scan(&self) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state.pending.clear();
        self.shared.cancel.store(true, Ordering::Release);
    }

    /// Reject future requests and cooperatively stop this host's worker.
    pub fn close(&self) {
        let mut state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        state.snapshot.closed = true;
        state.pending.clear();
        self.shared.cancel.store(true, Ordering::Release);
    }

    /// Blocking join. Never call this from an event/UI thread.
    pub fn shutdown(&self) {
        self.close();
        if let Some(worker) = self.worker.lock().unwrap_or_else(|e| e.into_inner()).take() {
            let _ = worker.join();
        }
    }

    pub fn wait_idle(&self, timeout: Duration) -> bool {
        let state = self.shared.state.lock().unwrap_or_else(|e| e.into_inner());
        let (state, _) = self
            .shared
            .idle
            .wait_timeout_while(state, timeout, |state| state.snapshot.progress.running)
            .unwrap_or_else(|e| e.into_inner());
        !state.snapshot.progress.running
    }
}

impl Drop for LibraryService {
    fn drop(&mut self) {
        // Do not block the UI's last Arc drop. The detached worker owns only its
        // captured old host roots and closed Shared, never a new host's state.
        self.close();
    }
}

fn run(shared: &Shared) {
    loop {
        let folder = {
            let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
            let next = if state.snapshot.closed {
                None
            } else {
                state.pending.pop()
            };
            let Some((next, background)) = next else {
                state.snapshot.progress.running = false;
                state.snapshot.progress.file.clear();
                shared.idle.notify_all();
                return;
            };
            let job = state.snapshot.progress.job + 1;
            state.snapshot.progress = ScanProgress {
                running: true,
                job,
                ..Default::default()
            };
            state.source_base = 0;
            state.background = background;
            // Same lock as cancellation: no request can reset a cancellation
            // intended for the already-started job.
            shared.cancel.store(false, Ordering::Release);
            next
        };
        let mut content_changed = true;
        let result = shared.store.open_or_create().and_then(|db| {
            let ids = folder.map(|id| vec![id]);
            let count = db
                .get_folders_with_metadata()?
                .iter()
                .filter(|f| f.enabled && folder.is_none_or(|id| f.id == id))
                .count();
            shared
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .snapshot
                .progress
                .source_count = count.min(u32::MAX as usize) as u32;
            std::fs::create_dir_all(&shared.artwork)?;
            db.with_connection(crate::scan_changes::begin)
                .map_err(|error| LibraryError::Database(error.to_string()))?;
            let result = crate::scan_with_progress(
                &db,
                ids.as_deref(),
                &shared.artwork,
                &shared.cancel,
                &|event| on_event(shared, event),
            );
            content_changed = db.with_connection(crate::scan_changes::changed)
                .map_err(|error| LibraryError::Database(error.to_string()))?;
            result
        });
        let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(error) = result {
            log::error!("library scan failed: {error}");
            let job = state.snapshot.progress.job;
            let background = state.background;
            state.record_completion(ScanCompletion {
                job,
                background,
                outcome: ScanOutcome::Failed,
                skipped: 0,
            });
        }
        state.snapshot.revision += 1;
        if content_changed {
            state.snapshot.content_revision += 1;
        }
    }
}

fn on_event(shared: &Shared, event: ScanEvent) {
    #[cfg(test)]
    if let Some(observer) = shared.observer.lock().unwrap().as_ref() {
        observer(&event);
    }
    let mut state = shared.state.lock().unwrap_or_else(|e| e.into_inner());
    let base = state.source_base;
    let progress = &mut state.snapshot.progress;
    match event {
        ScanEvent::TotalsAdded { total } => {
            progress.total = total;
            progress.source_total = total.saturating_sub(base);
        }
        ScanEvent::FileStarted { path } => {
            progress.file = Path::new(&path)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
        }
        ScanEvent::FileDone { processed, total } => {
            progress.processed = processed;
            progress.total = total;
            progress.source_processed = processed.saturating_sub(base);
            progress.source_total = total.saturating_sub(base);
        }
        ScanEvent::RootStarted { root_id, .. } => {
            progress.current_root_id = root_id;
            progress.source_processed = 0;
            progress.source_total = 0;
            progress.source_index += 1;
            state.source_base = progress.total;
        }
        ScanEvent::RootFinished {
            root_id,
            discovered,
            ..
        } => {
            let total = progress.total;
            if progress.current_root_id != root_id {
                progress.current_root_id = root_id;
                progress.source_index += 1;
            }
            progress.source_processed = discovered.min(u32::MAX as u64) as u32;
            progress.source_total = progress.source_processed;
            state.source_base = total;
        }
        ScanEvent::Cleanup => {
            progress.cleaning = true;
            progress.file.clear();
        }
        ScanEvent::Finished { status, errors } => {
            progress.cleaning = false;
            progress.file.clear();
            let job = progress.job;
            let background = state.background;
            state.record_completion(ScanCompletion {
                job,
                background,
                outcome: match status {
                    ScanStatus::Complete => ScanOutcome::Complete,
                    ScanStatus::Cancelled => ScanOutcome::Cancelled,
                    _ => ScanOutcome::Failed,
                },
                skipped: errors.len(),
            });
        }
        ScanEvent::Started => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(dir: &Path) -> LibraryService {
        LibraryService::new(
            LibraryStore::new(dir.join("library.db")),
            dir.join("artwork"),
        )
    }

    #[test]
    fn unchanged_and_missing_scans_complete_without_catalog_invalidation() {
        let temp = tempfile::tempdir().unwrap();
        let music = temp.path().join("music");
        std::fs::create_dir(&music).unwrap();
        let track = music.join("silence.wav");
        let mut wav = Vec::new();
        wav.extend_from_slice(b"RIFF");
        wav.extend_from_slice(&918_u32.to_le_bytes());
        wav.extend_from_slice(b"WAVEfmt ");
        wav.extend_from_slice(&16_u32.to_le_bytes());
        for value in [1_u16, 1] { wav.extend_from_slice(&value.to_le_bytes()); }
        for value in [44_100_u32, 88_200] { wav.extend_from_slice(&value.to_le_bytes()); }
        for value in [2_u16, 16] { wav.extend_from_slice(&value.to_le_bytes()); }
        wav.extend_from_slice(b"data");
        wav.extend_from_slice(&882_u32.to_le_bytes());
        wav.resize(926, 0);
        std::fs::write(&track, wav).unwrap();
        let service = host(&temp.path().join("profile"));
        let folder = service.register_folder(&music, 0).unwrap();
        let scan = || {
            assert!(service.scan_background(Some(folder.id)).unwrap());
            assert!(service.wait_idle(Duration::from_secs(10)));
            service.snapshot()
        };
        let imported = scan();
        assert_eq!(imported.content_revision, 2); // Registration + imported track.
        let reused = scan();
        assert_eq!(reused.content_revision, imported.content_revision);
        assert!(reused.revision > imported.revision);
        assert_eq!(reused.last_scan.unwrap().outcome, ScanOutcome::Complete);
        std::fs::remove_file(track).unwrap();
        let removed = scan();
        assert_eq!(removed.content_revision, imported.content_revision + 1);
        std::fs::remove_dir(music).unwrap();
        let missing = scan();
        assert_eq!(missing.content_revision, removed.content_revision);
        assert!(missing.last_scan.unwrap().skipped > 0);
        service.shutdown();
    }

    #[test]
    fn folder_registration_is_revisioned_idempotent_and_profile_owned() {
        let tmp = tempfile::tempdir().unwrap();
        let a = host(&tmp.path().join("a"));
        let b = host(&tmp.path().join("b"));
        let music = tmp.path().join("music");
        std::fs::create_dir(&music).unwrap();
        assert!(matches!(
            a.register_folder(Path::new("relative"), 0),
            Err(FolderError::InvalidPath)
        ));
        assert!(!a.store().database_path().exists());
        let folder = a.register_folder(&music, 0).unwrap();
        assert_eq!(a.snapshot().revision, 1);
        assert!(matches!(
            a.register_folder(&music, 0),
            Err(FolderError::RevisionChanged)
        ));
        assert_eq!(
            a.register_folder(&music.join("."), 1).unwrap().id,
            folder.id
        );
        assert_eq!(a.snapshot().revision, 1);
        assert_eq!(
            a.store()
                .read(|db| db.get_folders())
                .unwrap()
                .unwrap()
                .len(),
            1
        );
        let nested = music.join("album");
        std::fs::create_dir(&nested).unwrap();
        assert_eq!(a.register_folder(&nested, 1).unwrap().id, folder.id);
        assert!(matches!(
            a.register_folder(tmp.path(), 1),
            Err(FolderError::Conflict)
        ));
        a.store()
            .write(|db| db.set_folder_enabled(folder.id, false))
            .unwrap();
        assert!(matches!(
            a.register_folder(&nested, 1),
            Err(FolderError::Disabled)
        ));
        assert!(
            !a.store()
                .read(|db| db.get_folder_by_id(folder.id))
                .unwrap()
                .unwrap()
                .unwrap()
                .enabled
        );
        assert!(!b.store().database_path().exists());
        assert_eq!(b.register_folder(&music, 0).unwrap().id, folder.id);
        a.shutdown();
        assert!(matches!(
            a.register_folder(&music, 1),
            Err(FolderError::Closed)
        ));
        assert!(!b.snapshot().closed);
        b.shutdown();
    }

    #[test]
    fn queued_scans_coalesce_without_losing_directed_roots() {
        let mut queue = PendingScans::default();
        for id in [9, 4, 9] {
            queue.push(Some(id));
        }
        assert_eq!(queue.pop(), Some((Some(4), false)));
        assert_eq!(queue.pop(), Some((Some(9), false)));
        queue.push(Some(4));
        queue.push(None);
        queue.push(Some(9));
        assert_eq!(queue.pop(), Some((None, false)));
        assert_eq!(queue.pop(), None);
    }

    #[test]
    fn background_scans_keep_manual_intent_when_requests_coalesce() {
        let mut queue = PendingScans::default();
        queue.push_with_background(Some(1), true);
        queue.push(Some(1));
        queue.push_with_background(Some(2), true);
        assert_eq!(queue.pop(), Some((Some(1), false)));
        assert_eq!(queue.pop(), Some((Some(2), true)));
        queue.push(Some(1));
        queue.push_with_background(None, true);
        assert_eq!(queue.pop(), Some((None, false)));
        queue.push_with_background(None, true);
        queue.push(Some(2));
        assert_eq!(queue.pop(), Some((None, false)));
        queue.push_with_background(None, true);
        assert_eq!(queue.pop(), Some((None, true)));
    }

    #[test]
    fn background_completion_remains_silent_and_next_manual_scan_is_reportable() {
        let temp = tempfile::tempdir().unwrap();
        let service = host(temp.path());
        // A missing root reproduces the periodic "one skipped" completion.
        service
            .store()
            .open_or_create()
            .unwrap()
            .add_folder(temp.path().join("missing").to_str().unwrap())
            .unwrap();
        for background in [true, true, false, true] {
            assert!(service.scan_with_background(None, background).unwrap());
            assert!(service.wait_idle(Duration::from_secs(10)));
            let result = service.snapshot().last_scan.unwrap();
            assert_eq!(result.background, background);
            assert_eq!(result.skipped, 1);
            assert_eq!(result.outcome, ScanOutcome::Complete);
            if result.job >= 3 {
                assert_eq!(service.snapshot().last_requested_scan.unwrap().job, 3);
            } else {
                assert!(service.snapshot().last_requested_scan.is_none());
            }
        }
        service.shutdown();
    }

    #[test]
    fn separate_hosts_scan_real_roots_and_keep_cancellation_local() {
        let tmp = tempfile::tempdir().unwrap();
        let a = host(&tmp.path().join("a"));
        let b = host(&tmp.path().join("b"));
        assert!(!tmp.path().join("a").exists());
        for (service, name) in [(&a, "a"), (&b, "b")] {
            let music = tmp.path().join(format!("music-{name}"));
            std::fs::create_dir_all(&music).unwrap();
            service
                .store()
                .write(|db| {
                    assert!(matches!(
                        db.register_or_refresh_folder(&music),
                        crate::RegisterFolderOutcome::Added { .. }
                    ));
                    Ok(())
                })
                .unwrap();
        }
        a.close();
        assert!(!a.scan(None).unwrap());
        assert!(b.scan(None).unwrap());
        assert!(b.wait_idle(Duration::from_secs(10)));
        assert_eq!(
            b.snapshot().last_scan.unwrap().outcome,
            ScanOutcome::Complete
        );
        assert_eq!(b.snapshot().revision, 1);
        assert_eq!(a.snapshot().revision, 0);
        assert!(!a.artwork_directory().exists());
        assert!(b.artwork_directory().exists());
        a.shutdown();
        b.shutdown();
    }

    #[test]
    fn failed_open_is_terminal_and_worker_can_accept_another_request() {
        let tmp = tempfile::tempdir().unwrap();
        let parent = tmp.path().join("parent");
        std::fs::write(&parent, b"file").unwrap();
        let service = host(&parent);
        for job in 1..=2 {
            assert!(service.scan(None).unwrap());
            assert!(service.wait_idle(Duration::from_secs(5)));
            let snapshot = service.snapshot();
            assert_eq!(
                snapshot.last_scan.unwrap(),
                ScanCompletion {
                    background: false,
                    job,
                    outcome: ScanOutcome::Failed,
                    skipped: 0
                }
            );
            assert_eq!(snapshot.revision, job);
        }
        service.shutdown();
        assert!(!service.scan(None).unwrap());
    }

    #[test]
    fn cancel_clears_only_existing_queue_and_progress_does_not_expose_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let service = host(tmp.path());
        {
            let mut state = service.shared.state.lock().unwrap();
            state.pending.push(Some(42));
        }
        on_event(
            &service.shared,
            ScanEvent::FileStarted {
                path: "/private/host/music/song.flac".into(),
            },
        );
        assert_eq!(service.snapshot().progress.file, "song.flac");
        service.cancel_scan();
        assert_eq!(service.snapshot().queued, 0);
        assert!(service.shared.cancel.load(Ordering::Acquire));
        assert!(!service.snapshot().closed);
    }
    #[test]
    fn cancel_active_scan_discards_old_queue_but_keeps_new_request() {
        use std::sync::mpsc;
        let tmp = tempfile::tempdir().unwrap();
        let service = host(&tmp.path().join("profile"));
        let music = tmp.path().join("music");
        std::fs::create_dir_all(&music).unwrap();
        service
            .store()
            .write(|db| {
                db.register_or_refresh_folder(&music);
                Ok(())
            })
            .unwrap();
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let release_rx = Mutex::new(release_rx);
        let first = AtomicBool::new(true);
        *service.shared.observer.lock().unwrap() = Some(Box::new(move |event| {
            if matches!(event, ScanEvent::Started) && first.swap(false, Ordering::SeqCst) {
                entered_tx.send(()).unwrap();
                release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap();
            }
        }));
        service.scan(None).unwrap();
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        service.scan(Some(99)).unwrap();
        let job = service.snapshot().progress.job;
        assert!(!service.cancel_job(job + 1));
        assert_eq!(service.snapshot().queued, 1);
        assert!(matches!(
            service.register_folder(&music, 0),
            Err(FolderError::Busy)
        ));
        assert!(service.cancel_job(job));
        assert_eq!(service.snapshot().queued, 0);
        service.scan(None).unwrap();
        release_tx.send(()).unwrap();
        assert!(service.wait_idle(Duration::from_secs(10)));
        let snapshot = service.snapshot();
        assert_eq!(snapshot.revision, 2);
        assert_eq!(
            snapshot.last_scan.unwrap(),
            ScanCompletion {
                background: false,
                job: 2,
                outcome: ScanOutcome::Complete,
                skipped: 0,
            }
        );
        service.shutdown();
    }

    #[test]
    fn shutdown_joins_old_worker_and_never_runs_queued_scans() {
        use std::sync::mpsc;
        let tmp = tempfile::tempdir().unwrap();
        let service = Arc::new(host(&tmp.path().join("old")));
        let music = tmp.path().join("music");
        std::fs::create_dir_all(&music).unwrap();
        service
            .store()
            .write(|db| {
                db.register_or_refresh_folder(&music);
                Ok(())
            })
            .unwrap();
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let release_rx = Mutex::new(release_rx);
        *service.shared.observer.lock().unwrap() = Some(Box::new(move |event| {
            if matches!(event, ScanEvent::Started) {
                entered_tx.send(()).unwrap();
                release_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap();
            }
        }));
        service.scan(None).unwrap();
        entered_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        service.scan(None).unwrap();
        service.close();
        let joining = service.clone();
        let join = std::thread::spawn(move || joining.shutdown());
        assert!(!service.scan(Some(1)).unwrap());
        release_tx.send(()).unwrap();
        join.join().unwrap();
        let snapshot = service.snapshot();
        assert!(!snapshot.progress.running);
        assert_eq!(snapshot.revision, 1);
        assert_eq!(snapshot.queued, 0);
        assert_eq!(snapshot.last_scan.unwrap().outcome, ScanOutcome::Cancelled);
    }
}
