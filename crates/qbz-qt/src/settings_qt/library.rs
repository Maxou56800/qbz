//! Settings > Local Library — the folder table, the scan engine, maintenance,
//! the danger zone, and the Plex fields the panel needs on top of what the
//! `QbzLocal` bridge already publishes.
//!
//! ADR-006: nothing here re-implements scanning, folder identity or Plex.
//! Folder CRUD is `qbz_library::LibraryDatabase`; the scan is
//! `qbz_library::scan_with_progress` (the SAME engine the Slint frontend
//! drives, cancellation and cleanup included); Plex rides `crate::local_plex`
//! (wired this round) and the `QbzLocal` invokables.
//!
//! The per-folder editor (alias + network overrides) and the Plex PIN sign-in
//! flow are both live in Qt. Their surfaces are `LibFolderEditModal.qml` and
//! `PlexSettings.qml`; this module owns the former's persisted state while
//! `plex_pin_qt` owns the latter's pairing lifecycle.
//!
//! Retired 2026-08-04: "Add folder is a path field because this port has no
//! `rfd` dependency". It has had one since the MyQBZ round (`Cargo.toml:137`)
//! and three files were already calling it while this note said otherwise —
//! the path field now sits beside a real native chooser
//! ([`pick_and_add_folder`]). Kept as a record because the note was load
//! bearing: it justified a downgrade for weeks after its premise expired.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};

use qbz_library::LibraryDatabase;
use serde::Serialize;

// ---------------------------------------------------------------------------
// Transport rows (the `library` sub-document of the settings snapshot)
// ---------------------------------------------------------------------------

#[derive(Clone, Serialize)]
pub struct FolderRow {
    pub id: i64,
    #[serde(rename = "displayName")]
    pub display_name: String,
    pub path: String,
    /// Unix seconds; 0 = never scanned. QML formats it (locale-aware).
    #[serde(rename = "lastScan")]
    pub last_scan: i64,
    pub enabled: bool,
    #[serde(rename = "isNetwork")]
    pub is_network: bool,
    /// None until probed; local and network folders share the same status.
    pub accessible: Option<bool>,
    pub status: &'static str,
}

#[derive(Clone, Default, Serialize)]
pub struct PlexFields {
    /// The persisted (resolved) server url — prefills the address field.
    #[serde(rename = "serverUrl")]
    pub server_url: String,
    /// A token is stored (the token itself NEVER leaves Rust).
    #[serde(rename = "hasToken")]
    pub has_token: bool,
    #[serde(rename = "metadataWrite")]
    pub metadata_write: bool,
    /// Section collapse state (ui_prefs, same idea as the scrobbler header).
    pub collapsed: bool,
}

/// One media server's panel state.
///
/// The credential itself NEVER leaves Rust — the panel gets `hasCredential`
/// and nothing else, the same discipline `PlexFields` keeps with its token.
/// A password round-tripping through a QML string property would sit in the
/// scene graph's memory and in any JSON the panel logged.
/// Read one media server's persisted settings into the panel's shape.
fn media_fields(kind: qbz_app::settings::media_servers::MediaServerKind) -> MediaServerFields {
    let s = crate::media_servers_qt::get(kind);
    MediaServerFields {
        enabled: s.enabled,
        collapsed: s.ui_collapsed,
        server_url: s.base_url.clone(),
        // Jellyfin stores the USER ID here (every /Items call keys on it), so
        // it is not a name worth prefilling. Subsonic stores the real account
        // name and sends it on every request.
        username: match kind {
            qbz_app::settings::media_servers::MediaServerKind::Subsonic => s.username.clone(),
            _ => String::new(),
        },
        has_credential: s.is_configured(kind),
        server_name: s.server_name.clone(),
        cached_tracks: s.last_sync_tracks,
        last_sync_at: s.last_sync_at,
    }
}

#[derive(Clone, Default, Serialize)]
pub struct MediaServerFields {
    pub enabled: bool,
    pub collapsed: bool,
    /// The persisted address — prefills the field.
    #[serde(rename = "serverUrl")]
    pub server_url: String,
    /// The account name. Not secret, and prefilling it saves a retype.
    pub username: String,
    /// A usable credential is stored (a Jellyfin token, or a Subsonic
    /// password). What KIND it is differs per protocol and the panel does not
    /// need to know.
    #[serde(rename = "hasCredential")]
    pub has_credential: bool,
    /// What the server called itself at connect time ("blitzmini 10.11.11",
    /// "navidrome 1.16.1"). Empty until a successful connect.
    #[serde(rename = "serverName")]
    pub server_name: String,
    /// Tracks cached by the last completed sweep.
    #[serde(rename = "cachedTracks")]
    pub cached_tracks: i64,
    /// Unix seconds of that sweep; 0 = never.
    #[serde(rename = "lastSyncAt")]
    pub last_sync_at: i64,
}

#[derive(Clone, Default, Serialize)]
pub struct Snapshot {
    pub folders: Vec<FolderRow>,
    pub picked_path: String,
    pub scanning: bool,
    pub processed: i32,
    pub total: i32,
    /// Progress within the currently scanned folder/root. These counters are
    /// derived from the streaming scanner's global offsets; no second tree
    /// walk is introduced merely to paint UI.
    #[serde(rename = "sourceProcessed")]
    pub source_processed: i32,
    #[serde(rename = "sourceTotal")]
    pub source_total: i32,
    #[serde(rename = "currentRootId")]
    pub current_root_id: i64,
    #[serde(rename = "sourceIndex")]
    pub source_index: i32,
    #[serde(rename = "sourceCount")]
    pub source_count: i32,
    pub file: String,
    pub cleaning: bool,
    #[serde(rename = "cleanupStatus")]
    pub cleanup_status: String,
    pub clearing: bool,
    /// Inline result of the last add/remove (empty when there is nothing to say).
    pub status: String,
    pub plex: PlexFields,
    /// Jellyfin and Subsonic. One shape twice, because the panel is one form
    /// twice — see `qbz_app::settings::media_servers`.
    pub jellyfin: MediaServerFields,
    pub subsonic: MediaServerFields,
    /// The per-folder settings modal (`LibFolderEditModal.slint`). Rides the
    /// settings document rather than a bridge of its own — it is a surface of
    /// this panel and every one of its actions already lands in this module.
    #[serde(rename = "folderEdit")]
    pub folder_edit: FolderEdit,
}

/// `LibFolderEditState`. `open == false` is the closed shape, and the FULL
/// shape is always published (never `{}`) so QML never parses a half-object
/// in the frame before the first open.
#[derive(Clone, Default, Serialize)]
pub struct FolderEdit {
    pub open: bool,
    #[serde(rename = "folderId")]
    pub folder_id: i64,
    pub path: String,
    pub alias: String,
    pub enabled: bool,
    #[serde(rename = "isNetwork")]
    pub is_network: bool,
    /// The user has taken manual control of the network flag — this is the
    /// field with a real consumer at `qbz-library/src/scan.rs:164`, where it
    /// suppresses per-scan network re-detection. With no writer (the state
    /// this port was in until 2026-08-05) a user's classification was
    /// overwritten by detection on every single scan.
    #[serde(rename = "userOverrideNetwork")]
    pub user_override_network: bool,
    /// Index into the modal's option list; 0 = auto-detect.
    #[serde(rename = "fsTypeIndex")]
    pub fs_type_index: i32,
    pub accessible: bool,
    #[serde(rename = "checkingAccessible")]
    pub checking_accessible: bool,
    /// Unix seconds; 0 = never scanned (QML formats it, locale-aware).
    #[serde(rename = "lastScan")]
    pub last_scan: i64,
}

/// The modal's fs-type option list, in the reference's order
/// (`LibFolderEditModal.slint:266-278`). Index 0 means "let detection
/// decide" and is stored as no explicit type.
const FS_TYPE_VALUES: &[&str] = &[
    "auto",
    "cifs",
    "nfs",
    "sshfs",
    "rclone",
    "webdav",
    "glusterfs",
    "ceph",
    "other",
];

fn fs_type_index(label: Option<&str>) -> i32 {
    let Some(label) = label else {
        return 0;
    };
    FS_TYPE_VALUES
        .iter()
        .position(|v| v.eq_ignore_ascii_case(label))
        .map(|i| i as i32)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Live state
// ---------------------------------------------------------------------------

static CLEANING: AtomicBool = AtomicBool::new(false);
static CLEANUP_STATUS: LazyLock<Mutex<String>> = LazyLock::new(|| Mutex::new(String::new()));
static CLEARING: AtomicBool = AtomicBool::new(false);
static STATUS: LazyLock<Mutex<String>> = LazyLock::new(|| Mutex::new(String::new()));

/// Last known accessibility per folder id and path, filled by
/// [`spawn_accessibility_probes`].
///
/// It exists because the probe cannot run where it used to. `snapshot()` is
/// called on the settings publish path — every publish, and every 2 s while a
/// scan is running — and it used to `std::fs::read_dir` each network folder
/// INLINE. One dead NFS/SMB mount then blocked the whole settings document
/// behind the kernel's mount timeout, which can be tens of seconds. The
/// reference never had that: it publishes `accessible: true` optimistically
/// and updates each row from a spawned probe with a 6 s ceiling
/// (`crates/qbz/src/local_library_settings.rs:212-236`).
static ACCESSIBLE: LazyLock<Mutex<std::collections::HashMap<(i64, String), FolderAccess>>> =
    LazyLock::new(|| Mutex::new(std::collections::HashMap::new()));

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FolderAccess { Available, Missing, Disconnected, Denied, Unavailable }
impl FolderAccess {
    fn from_error(error: std::io::ErrorKind) -> Self {
        use std::io::ErrorKind::*;
        match error {
            NotFound | NotADirectory => Self::Missing,
            PermissionDenied => Self::Denied,
            NotConnected | ConnectionAborted | ConnectionReset | ConnectionRefused
                | NetworkUnreachable | HostUnreachable => Self::Disconnected,
            _ => Self::Unavailable,
        }
    }
    fn label(self, enabled: bool) -> &'static str {
        match self {
            Self::Available if enabled => "active",
            Self::Available => "hidden",
            Self::Missing => "missing",
            Self::Disconnected => "disconnected",
            Self::Denied => "denied",
            Self::Unavailable => "unavailable",
        }
    }
}
static PICKED_PATH: LazyLock<Mutex<String>> = LazyLock::new(|| Mutex::new(String::new()));

fn set_status(text: String) {
    *STATUS.lock().unwrap_or_else(|e| e.into_inner()) = text;
}

/// Open (creating if needed) the per-user `library.db`.
///
/// `local_state::with_db` deliberately returns `None` when the file does not
/// exist yet (a browse read must never create an empty library); adding the
/// FIRST folder is exactly the case that has to create it, so this module
/// owns a creating opener.
fn open_db() -> Option<LibraryDatabase> {
    let store = qbz_library::LibraryStore::new(crate::local_state::db_path()?);
    match store.open_or_create() {
        Ok(db) => Some(db),
        Err(e) => {
            log::error!("[qbz-qt] library db open failed: {e}");
            None
        }
    }
}

fn basename(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

// ---------------------------------------------------------------------------
// Snapshot (called from the settings publish, on the blocking pool)
// ---------------------------------------------------------------------------

pub fn snapshot() -> Snapshot {
    let folders = crate::local_state::with_db(|db| db.get_folders_with_metadata())
        .unwrap_or_default()
        .into_iter()
        .map(|f| {
            // Do not block settings publication on filesystem I/O. Unknown
            // paths remain visibly unchecked until the background probe finishes.
            let accessible = ACCESSIBLE.lock().unwrap_or_else(|e| e.into_inner())
                .get(&(f.id, f.path.clone())).copied();
            FolderRow {
                display_name: match f.alias.as_deref() {
                    Some(a) if !a.is_empty() => a.to_string(),
                    _ => basename(&f.path),
                },
                id: f.id,
                path: f.path,
                last_scan: f.last_scan.unwrap_or(0),
                enabled: f.enabled,
                is_network: f.is_network,
                accessible: accessible.map(|a| a == FolderAccess::Available),
                status: accessible.map(|a| a.label(f.enabled)).unwrap_or("checking"),
            }
        })
        .collect();

    let progress = crate::local_service_qt::current()
        .map(|h| h.service.snapshot().progress)
        .unwrap_or_default();
    let plex_cfg = crate::local_plex::settings();
    Snapshot {
        folders,
        picked_path: PICKED_PATH.lock().unwrap_or_else(|e| e.into_inner()).clone(),
        scanning: progress.running,
        processed: progress.processed as i32,
        total: progress.total as i32,
        source_processed: progress.source_processed as i32,
        source_total: progress.source_total as i32,
        current_root_id: progress.current_root_id,
        source_index: progress.source_index as i32,
        source_count: progress.source_count as i32,
        file: if progress.cleaning {
            qbz_i18n::t("Cleaning up missing files...")
        } else {
            progress.file
        },
        cleaning: CLEANING.load(Ordering::SeqCst),
        cleanup_status: CLEANUP_STATUS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone(),
        clearing: CLEARING.load(Ordering::SeqCst),
        status: STATUS.lock().unwrap_or_else(|e| e.into_inner()).clone(),
        jellyfin: media_fields(qbz_app::settings::media_servers::MediaServerKind::Jellyfin),
        subsonic: media_fields(qbz_app::settings::media_servers::MediaServerKind::Subsonic),
        plex: PlexFields {
            has_token: !plex_cfg.token.trim().is_empty(),
            server_url: plex_cfg.base_url,
            metadata_write: plex_cfg.metadata_write_enabled,
            collapsed: super::pref_bool("plex_ui_collapsed", false),
        },
        folder_edit: EDIT.lock().unwrap_or_else(|e| e.into_inner()).clone(),
    }
}

// ---------------------------------------------------------------------------
// Per-folder settings modal (LibFolderEditModal)
// ---------------------------------------------------------------------------

static EDIT: LazyLock<Mutex<FolderEdit>> = LazyLock::new(|| Mutex::new(FolderEdit::default()));

/// Open the modal on one folder: seed it from the DB row, then probe the
/// mount in the background.
///
/// The probe is deliberately NOT awaited before publishing — the modal opens
/// on the row we already have and the accessibility line resolves under it,
/// which is the reference's behaviour (`checking-accessible` starts true and
/// `check_accessible` clears it). Opening behind a dead NFS mount would
/// otherwise take the kernel's mount timeout.
pub async fn open_folder_edit(id: i64) {
    let row = tokio::task::spawn_blocking(move || {
        crate::local_state::with_db(|db| db.get_folders_with_metadata())
            .unwrap_or_default()
            .into_iter()
            .find(|f| f.id == id)
    })
    .await
    .ok()
    .flatten();
    let Some(f) = row else {
        log::warn!("[qbz-qt] folder edit: id {id} is gone");
        return;
    };
    {
        let mut st = EDIT.lock().unwrap_or_else(|e| e.into_inner());
        *st = FolderEdit {
            open: true,
            folder_id: f.id,
            path: f.path.clone(),
            alias: f.alias.clone().unwrap_or_default(),
            enabled: f.enabled,
            is_network: f.is_network,
            user_override_network: f.user_override_network,
            fs_type_index: fs_type_index(f.network_fs_type.as_deref()),
            // Optimistic, like the row list — the probe below corrects it.
            accessible: true,
            checking_accessible: true,
            last_scan: f.last_scan.unwrap_or(0),
        };
    }
    super::publish_snapshot().await;

    let path = f.path.clone();
    let accessible = probe_path(&path).await;
    {
        let mut st = EDIT.lock().unwrap_or_else(|e| e.into_inner());
        // The user may have closed the modal or opened a DIFFERENT folder
        // while the probe was out; writing either would show a stale answer.
        if !st.open || st.folder_id != id {
            return;
        }
        st.accessible = accessible == FolderAccess::Available;
        st.checking_accessible = false;
    }
    record_accessible(id, &path, accessible);
    super::publish_snapshot().await;
}

/// All filesystem access stays on the blocking pool, including local paths
/// that may actually sit on a stale mount. Failure/timeout means unavailable
/// now, never proof that a folder should be removed from the library.
async fn probe_path(path: &str) -> FolderAccess {
    let path = path.to_owned();
    match tokio::time::timeout(
        std::time::Duration::from_secs(6),
        tokio::task::spawn_blocking(move || std::fs::read_dir(path).map(|_| ())),
    ).await {
        Ok(Ok(Ok(()))) => FolderAccess::Available,
        Ok(Ok(Err(error))) => FolderAccess::from_error(error.kind()),
        _ => FolderAccess::Unavailable,
    }
}

pub async fn close_folder_edit() {
    EDIT.lock().unwrap_or_else(|e| e.into_inner()).open = false;
    super::publish_snapshot().await;
}

/// Save the modal. `fs_type` is the STRING (the QML maps its index through
/// the same table); "auto" on a network folder means "re-detect the label
/// from the path", which is what the reference does at
/// `local_library_settings.rs:435-439`.
pub async fn save_folder_edit(
    id: i64,
    alias: String,
    enabled: bool,
    is_network: bool,
    fs_type: String,
    user_override: bool,
) {
    let path = EDIT.lock().unwrap_or_else(|e| e.into_inner()).path.clone();
    let ok = tokio::task::spawn_blocking(move || {
        let fs_opt: Option<String> = if !is_network {
            None
        } else if fs_type == "auto" {
            qbz_library::network_fs_label(std::path::Path::new(&path))
        } else {
            Some(fs_type)
        };
        let alias_trimmed = alias.trim().to_string();
        let alias_opt = if alias_trimmed.is_empty() {
            None
        } else {
            Some(alias_trimmed)
        };
        crate::local_state::with_db(|db| {
            db.update_folder_settings(
                id,
                alias_opt.as_deref(),
                enabled,
                is_network,
                fs_opt.as_deref(),
                user_override,
            )
        })
        .is_some()
    })
    .await
    .unwrap_or(false);

    if !ok {
        set_status(qbz_i18n::t("Could not save the folder settings."));
        super::publish_snapshot().await;
        return;
    }
    EDIT.lock().unwrap_or_else(|e| e.into_inner()).open = false;
    crate::toast_qt::success(qbz_i18n::t("Folder settings saved"));
    refresh_browse();
    super::publish_snapshot().await;
}

/// "Change" next to the path: pick a new location for an EXISTING folder,
/// keeping its id (and therefore its tracks' folder association).
///
/// The DB refuses a path already registered to a different folder, which is
/// the one failure the reference surfaces by name.
pub async fn change_folder_path(id: i64) {
    let Some(dir) = rfd::AsyncFileDialog::new()
        .set_title(&qbz_i18n::t("Choose a music folder"))
        .pick_folder()
        .await
    else {
        return;
    };
    let new_path = dir.path().to_string_lossy().to_string();
    let np = new_path.clone();
    let ok = tokio::task::spawn_blocking(move || {
        crate::local_state::with_db(|db| db.update_folder_path(id, &np)).is_some()
    })
    .await
    .unwrap_or(false);

    if !ok {
        crate::toast_qt::error(qbz_i18n::t(
            "Couldn't change folder location (path may already exist)",
        ));
        return;
    }
    {
        let mut st = EDIT.lock().unwrap_or_else(|e| e.into_inner());
        if st.folder_id == id {
            st.path = new_path;
            // The new location has never been scanned under this id.
            st.last_scan = 0;
            st.checking_accessible = true;
        }
    }
    super::publish_snapshot().await;

    let path = EDIT.lock().unwrap_or_else(|e| e.into_inner()).path.clone();
    let accessible = probe_path(&path).await;
    record_accessible(id, &path, accessible);
    {
        let mut st = EDIT.lock().unwrap_or_else(|e| e.into_inner());
        if st.open && st.folder_id == id {
            st.accessible = accessible == FolderAccess::Available;
            st.checking_accessible = false;
        }
    }
    refresh_browse();
    super::publish_snapshot().await;
}

// ---------------------------------------------------------------------------
// Folder CRUD
// ---------------------------------------------------------------------------

/// Probe every configured folder off the publish path. Keep missing rows
/// so the user can reconnect storage, change the path or explicitly remove it.
pub async fn spawn_accessibility_probes() {
    let folders = tokio::task::spawn_blocking(|| {
        crate::local_state::with_db(|db| db.get_folders_with_metadata()).unwrap_or_default()
    })
    .await
    .unwrap_or_default();

    for f in folders {
        let accessible = probe_path(&f.path).await;
        if record_accessible(f.id, &f.path, accessible) {
            super::publish_snapshot().await;
        }
    }
}

/// Store one probe result; `true` when it differs from what was cached.
fn record_accessible(id: i64, path: &str, accessible: FolderAccess) -> bool {
    let mut map = ACCESSIBLE.lock().unwrap_or_else(|e| e.into_inner());
    map.insert((id, path.to_owned()), accessible) != Some(accessible)
}

/// The folder button fills the draft path. Both typed and browsed paths are
/// registered only when Add is pressed.
///
/// The reference does the same thing with the same crate
/// (`crates/qbz/src/local_library_settings.rs` -> `rfd::AsyncFileDialog`).
/// This port carried a note claiming it had no `rfd` dependency; it has had
/// one since the MyQBZ round (`Cargo.toml:137`, rfd 0.15) and three other
/// files already call it. Cancelling the dialog is a no-op, not an error.
pub async fn pick_and_add_folder() {
    let Some(dir) = rfd::AsyncFileDialog::new()
        .set_title(&qbz_i18n::t("Choose a music folder"))
        .pick_folder()
        .await
    else {
        return;
    };
    *PICKED_PATH.lock().unwrap_or_else(|e| e.into_inner()) = dir.path().to_string_lossy().to_string();
    super::publish_snapshot().await;
}

/// "Add folder": canonicalize once and let Local Library decide whether to
/// add, refresh, reuse an ancestor, or reject an overlap.
pub async fn add_folder(path: String) {
    PICKED_PATH.lock().unwrap_or_else(|e| e.into_inner()).clear();
    let path = path.trim().to_string();
    if path.is_empty() {
        return;
    }
    let _ = tokio::task::spawn_blocking(move || {
        let Some(db) = open_db() else {
            set_status(qbz_i18n::t("Could not open the library database."));
            return;
        };
        match db.register_or_refresh_folder(std::path::Path::new(&path)) {
            qbz_library::RegisterFolderOutcome::Added { .. }
            | qbz_library::RegisterFolderOutcome::Refreshed { .. }
            | qbz_library::RegisterFolderOutcome::Covered { .. } => set_status(String::new()),
            qbz_library::RegisterFolderOutcome::RegisteredDisabled { .. } => {
                set_status(qbz_i18n::t("That library folder is disabled."));
            }
            qbz_library::RegisterFolderOutcome::Conflict => {
                set_status(qbz_i18n::t(
                    "That folder overlaps an existing library folder.",
                ));
            }
            qbz_library::RegisterFolderOutcome::Failed => {
                set_status(qbz_i18n::t("Could not add that folder."));
            }
        }
    })
    .await;
    refresh_browse();
    spawn_accessibility_probes().await;
}

/// Remove folders (their indexed tracks go with them — `remove_folder_with_tracks`).
pub async fn remove_folders(ids: Vec<i64>) {
    if ids.is_empty() {
        return;
    }
    let keys = tokio::task::spawn_blocking(move || {
        let Some(db) = open_db() else {
            return Vec::new();
        };
        let all = db.get_folders_with_metadata().unwrap_or_default();
        let mut keys: Vec<String> = Vec::new();
        for id in ids {
            let Some(folder) = all.iter().find(|f| f.id == id) else {
                continue;
            };
            // Capture the album keys BEFORE the delete — afterwards the rows
            // are gone and there is nothing left to match on. 1:1 with the
            // reference's comment at local_library_settings.rs:341-343.
            keys.extend(db.album_keys_in_folder(&folder.path).unwrap_or_default());
            if let Err(e) = db.remove_folder_with_tracks(&folder.path) {
                log::error!("[qbz-qt] remove folder failed: {e}");
                set_status(qbz_i18n::t("Could not remove that folder."));
            }
        }
        keys
    })
    .await
    .unwrap_or_default();
    // Recently Played is a SEPARATE store from the library database, so
    // removing the folder does not touch it. Until 2026-08-04 this port
    // skipped the prune entirely and the history kept cards that opened onto
    // deleted tracks.
    let removed = crate::recently_qt::prune_albums(&keys);
    if removed > 0 {
        log::info!("[qbz-qt] pruned {removed} recently-played entries with the removed folder(s)");
    }
    // "Most Played Albums" is a THIRD store (album_play_history.db) with the
    // same album keys; prune it too or its cards outlive the folder.
    let removed = qbz_app::settings::album_play_history::prune_albums(&keys);
    if removed > 0 {
        log::info!("[qbz-qt] pruned {removed} most-played entries with the removed folder(s)");
    }
    refresh_browse();
}

/// Flip a folder's enabled flag (a disabled folder is skipped by every scan).
pub async fn toggle_folder_enabled(id: i64) {
    let _ = tokio::task::spawn_blocking(move || {
        let Some(db) = open_db() else {
            return;
        };
        let current = db
            .get_folder_by_id(id)
            .ok()
            .flatten()
            .map(|f| f.enabled)
            .unwrap_or(true);
        if let Err(e) = db.set_folder_enabled(id, !current) {
            log::error!("[qbz-qt] set folder enabled failed: {e}");
        }
    })
    .await;
    refresh_browse();
}

// ---------------------------------------------------------------------------
// Scan
// ---------------------------------------------------------------------------

/// Queue work on the captured host. Qt only translates/publishes its snapshot;
/// all scheduling, cancellation and progress now belong to qbz-library.
pub fn scan(folder_id: Option<i64>) -> bool {
    let Some(host) = crate::local_service_qt::current() else {
        return false;
    };
    scan_for(host, folder_id, false)
}

/// Background observers carry the same captured binding as their root IDs.
/// A profile switch can close this host, but can never retarget the request.
pub(crate) fn scan_for(
    host: std::sync::Arc<crate::local_service_qt::DesktopLibrary>,
    folder_id: Option<i64>,
    background: bool,
) -> bool {
    if !crate::local_service_qt::is_current(&host) {
        return false;
    }
    let before = host.service.snapshot();
    let requested = if background {
        host.service.scan_background(folder_id)
    } else {
        host.service.scan(folder_id)
    };
    match requested {
        Ok(true) => {}
        Ok(false) => return false,
        Err(error) => {
            log::error!("[qbz-qt] library scan start failed: {error}");
            if !background {
                set_status(qbz_i18n::t("Scan failed."));
            }
            return false;
        }
    }
    if !host.begin_observing() {
        return true;
    }
    crate::spawn(async move {
        let mut revision = before.content_revision;
        let mut completed = before.last_requested_scan.map(|s| s.job).unwrap_or(0);
        loop {
            if !crate::local_service_qt::is_current(&host) {
                break;
            }
            let snapshot = host.service.snapshot();
            if let Some(result) = snapshot
                .last_requested_scan
                .as_ref()
                .filter(|s| s.job > completed)
            {
                completed = result.job;
                scan_finished(result);
            }
            if snapshot.content_revision != revision {
                revision = snapshot.content_revision;
                crate::local_catalog_qt::request_catch_up();
                refresh_browse();
            }
            if !snapshot.progress.running {
                CLEANING.store(false, Ordering::SeqCst);
            }
            super::publish_snapshot().await;
            if !snapshot.progress.running {
                host.stop_observing();
                // A request may arrive as this observer exits. Either it owns
                // the next ticker or this ticker resumes; no progress is lost.
                if !host.service.snapshot().progress.running || !host.begin_observing() {
                    return;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(750)).await;
        }
        host.stop_observing();
    });
    true
}

fn scan_finished(result: &qbz_library::service::ScanCompletion) {
    use qbz_library::service::ScanOutcome;
    if result.background {
        log::debug!(
            "[local-scan] background job {} settled: {:?}, {} skipped",
            result.job,
            result.outcome,
            result.skipped
        );
        return;
    }
    if CLEANING.load(Ordering::SeqCst) {
        *CLEANUP_STATUS.lock().unwrap_or_else(|e| e.into_inner()) = match result.outcome {
            ScanOutcome::Complete => qbz_i18n::t("Scan complete"),
            ScanOutcome::Cancelled => qbz_i18n::t("Scan cancelled"),
            ScanOutcome::Failed => qbz_i18n::t("Cleanup failed."),
        };
    }
    match result.outcome {
        ScanOutcome::Complete if result.skipped > 0 => crate::toast_qt::success(qbz_i18n::tf(
            "Scan complete ({} file skipped)",
            "Scan complete ({} files skipped)",
            result.skipped as i64,
            &[&result.skipped.to_string()],
        )),
        ScanOutcome::Complete => crate::toast_qt::success(qbz_i18n::t("Scan complete")),
        ScanOutcome::Cancelled => crate::toast_qt::success(qbz_i18n::t("Scan cancelled")),
        ScanOutcome::Failed => {
            set_status(qbz_i18n::t("Scan failed."));
            crate::toast_qt::error(qbz_i18n::t("Scan failed"));
        }
    }
}

pub fn stop_scan() {
    if let Some(host) = crate::local_service_qt::current() {
        host.service.cancel_scan();
    }
}

pub fn reset_profile_state() {
    CLEANING.store(false, Ordering::SeqCst);
    CLEARING.store(false, Ordering::SeqCst);
    CLEANUP_STATUS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
    STATUS.lock().unwrap_or_else(|e| e.into_inner()).clear();
    ACCESSIBLE.lock().unwrap_or_else(|e| e.into_inner()).clear();
    PICKED_PATH.lock().unwrap_or_else(|e| e.into_inner()).clear();
    *EDIT.lock().unwrap_or_else(|e| e.into_inner()) = FolderEdit::default();
}

// ---------------------------------------------------------------------------
// Maintenance + danger zone
// ---------------------------------------------------------------------------

/// Remove tracks whose files no longer exist (Slint `cleanup_missing`, same
/// guard: a network folder whose mount is DOWN stats as missing for every
/// file, so its subtree is skipped instead of being wiped).
pub async fn cleanup_missing() {
    if CLEANING.swap(true, Ordering::SeqCst) {
        return;
    }
    *CLEANUP_STATUS.lock().unwrap_or_else(|e| e.into_inner()) =
        qbz_i18n::t("Scanning track paths...");
    if !scan(None) {
        CLEANING.store(false, Ordering::SeqCst);
        *CLEANUP_STATUS.lock().unwrap_or_else(|e| e.into_inner()) = qbz_i18n::t("Scan failed.");
    }
    super::publish_snapshot().await;
}

/// "Clear library database" — drops the indexed tracks; the audio files and
/// the registered folders stay (`clear_all_tracks`, Slint parity).
pub async fn clear_library() {
    if CLEARING.swap(true, Ordering::SeqCst) {
        return;
    }
    super::publish_snapshot().await;
    let _ = tokio::task::spawn_blocking(|| {
        if crate::local_state::with_db(|db| db.clear_all_tracks()).is_none() {
            set_status(qbz_i18n::t("Could not clear the library database."));
        }
    })
    .await;
    CLEARING.store(false, Ordering::SeqCst);
    crate::local_catalog_qt::request_catch_up();
    refresh_browse();
    super::publish_snapshot().await;
}

// ---------------------------------------------------------------------------
// Plex (the rows the QbzLocal properties do not already carry)
// ---------------------------------------------------------------------------

pub fn set_metadata_write(value: bool) {
    crate::local_plex::set_metadata_write_enabled(value);
}

/// Plex danger zone > "Clear cache": drop the cached libraries and tracks but
/// KEEP the sign-in (`disconnect` is the one that clears credentials).
pub async fn plex_clear_cache() {
    let _ = tokio::task::spawn_blocking(|| {
        if let Err(e) = qbz_plex::plex_cache_clear() {
            log::error!("[qbz-qt] plex cache clear failed: {e}");
        }
    })
    .await;
    crate::local_catalog_qt::request_catch_up();
    crate::local_bridge_ops::publish_plex_state();
    refresh_browse();
}

// ---------------------------------------------------------------------------
// Shared tail
// ---------------------------------------------------------------------------

/// Re-run the Local Library browse documents after any mutation — the grid,
/// the artists rail, the Tracks page and the tab badges are all queries.
fn refresh_browse() {
    crate::local_bridge_ops::reload_browse();
}
