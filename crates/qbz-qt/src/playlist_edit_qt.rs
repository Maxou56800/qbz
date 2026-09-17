//! Shared playlist editor controller — the Rust half of `QbzPlaylistEdit`
//! (contract block 3). Port of `primitives/EditPlaylistModal.slint` +
//! the reference's `EditPlaylistActions` handlers.
//!
//! ONE editor for BOTH kinds of playlist and for every surface that offers
//! "Edit playlist": the manager's three delegates, the sidebar row menu and
//! the playlist detail header's pencil.
//!
//! # What is state here and what is state in QML
//!
//! Name, description and the offline-only checkbox are QML-LOCAL drafts;
//! `save(name, description, offlineOnly)` takes all three as arguments
//! (`MyQbzModals.qml`'s convention). What lives HERE is only what QML cannot
//! own:
//!   * the id being edited — held in Rust so a republish can never make the
//!     modal save under a different playlist;
//!   * `descLoaded` — a resolution OUTCOME, and the gate that decides whether
//!     a description write happens at all;
//!   * `isLocal` — the kind, which decides every branch below;
//!   * `busy` — it gates the writes, so the writer owns it.
//!
//! # The three rules a naive transcription gets wrong
//!
//! **1. A rename must CARRY the description (§5.2).** The reference seeds
//! `description: ""` from the manager and always sends `Some(trimmed)` into
//! `update_playlist`, so renaming a Qobuz playlist from the manager DELETES
//! its description; the local branch is the same story with SQL NULL
//! (`local_playlists.rs set_description(conn, id, None)`). Here the real
//! description is RESOLVED before the modal opens, and when it could not be
//! resolved (`desc_loaded == false`) the field is not rendered and `save`
//! passes `None` — which `update_playlist` reads as "leave it alone". A field
//! the user never saw can never overwrite stored data.
//!
//! **2. A Qobuz delete must RE-DERIVE ownership (§5.1).** `playlist/delete`
//! returns 200 and silently no-ops on a playlist you do not own — the
//! "deleted ok but it stays" bug. A FOLLOWED playlist goes through
//! `unsubscribe_playlist`. That branch lives in
//! `playlist_qt::delete_by_id`, which this module delegates to rather than
//! reimplementing.
//!
//! **3. The back-navigation after a delete is CONDITIONAL (§5.1).** Deleting
//! from the manager or from the sidebar must NOT navigate — an unconditional
//! `nav_qt::back()` throws the user off the Playlist Manager the moment they
//! delete a row. `playlist_qt::back_if_showing` fires only when the open
//! detail IS the deleted playlist.
//!
//! # Offline and account-less users are first-class here
//!
//! A `local:` id resolves, saves and deletes through pure `library.db` reads
//! and writes — no network, no Qobuz account. So none of that path may be
//! routed through `crate::reload_sidebar()`, which opens with an
//! `is_offline()` early return: every refresh here goes through
//! `crate::reload_sidebar_including_local()` (D10) plus
//! `playlist_manager_qt::reload_if_loaded()`, the ONE call allowed to
//! early-return on a cold manager cache (§5.3).
//!
//! # Thread rules (§5.19)
//!
//! `LibraryDatabase` wraps a `!Send` rusqlite connection, so every
//! `local_playlist_qt` call is `spawn_blocking`-only. An invokable mutates a
//! `Mutex`, publishes, and `crate::spawn`s the rest.

use std::sync::{LazyLock, Mutex};

use cxx_qt_lib::QString;
use serde::Serialize;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
struct EditState {
    open: bool,
    /// `""` while closed; otherwise `"12345678"` or `"local:<uuid>"`. Kept as
    /// a STRING all the way down — nothing may parse a `local:` id as an
    /// integer (§3.1).
    id: String,
    /// SEEDs, consumed once by QML on the open transition.
    name: String,
    description: String,
    /// `false` ⇒ the real description could not be resolved. The modal then
    /// hides the field and `save` passes `None` (§5.2).
    desc_loaded: bool,
    is_local: bool,
    offline_only: bool,
    busy: bool,
    /// The folder the playlist is filed in ("" = root) — the editor's
    /// "Folder" row (2026-09-13).
    folder_id: String,
}

static EDIT: LazyLock<Mutex<EditState>> = LazyLock::new(|| Mutex::new(EditState::default()));

/// The "Delete playlist?" confirmation summoned from a context menu (sidebar
/// row, playlist card) — the same delete the editor's button runs, minus the
/// editor. Published as `QbzPlaylistEdit.deleteJson`; the shell-level modal
/// self-gates on `open` (2026-09-13).
#[derive(Clone, Debug, Default)]
struct DeleteAsk {
    open: bool,
    id: String,
    name: String,
    is_local: bool,
}

static DELETE_ASK: LazyLock<Mutex<DeleteAsk>> =
    LazyLock::new(|| Mutex::new(DeleteAsk::default()));

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DeleteDoc {
    open: bool,
    id: String,
    name: String,
    is_local: bool,
}

pub(crate) fn publish_delete_ask() {
    let ask = DELETE_ASK.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let json = serde_json::to_string(&DeleteDoc {
        open: ask.open,
        id: ask.id,
        name: ask.name,
        is_local: ask.is_local,
    })
    .unwrap_or_else(|_| "{\"open\":false}".to_string());
    crate::playlist_edit_bridge::ui(move |mut b| {
        b.as_mut().set_delete_json(QString::from(json.as_str()));
    });
}

/// Open the confirmation for `id` (a Qobuz id or a local ref), showing `name`.
pub(crate) fn ask_delete(id: &str, name: &str) {
    let id = id.trim().to_string();
    if id.is_empty() {
        return;
    }
    let is_local = matches!(
        crate::local_playlist_qt::PlaylistRef::parse(&id),
        Some(crate::local_playlist_qt::PlaylistRef::Local(_))
    );
    {
        let mut ask = DELETE_ASK.lock().unwrap_or_else(|e| e.into_inner());
        *ask = DeleteAsk {
            open: true,
            id,
            name: name.to_string(),
            is_local,
        };
    }
    publish_delete_ask();
}

pub(crate) fn cancel_delete() {
    DELETE_ASK.lock().unwrap_or_else(|e| e.into_inner()).open = false;
    publish_delete_ask();
}

/// The modal's Delete: run the same delete the editor runs, refresh the
/// surfaces, and leave the detail page only if it was the deleted one.
pub(crate) fn confirm_delete() {
    let (id, is_local) = {
        let mut ask = DELETE_ASK.lock().unwrap_or_else(|e| e.into_inner());
        if !ask.open || ask.id.is_empty() {
            return;
        }
        ask.open = false;
        (std::mem::take(&mut ask.id), ask.is_local)
    };
    publish_delete_ask();
    crate::spawn(async move {
        let ok = delete_ref(&id, is_local).await;
        if ok {
            crate::playlist_qt::back_if_showing(&id);
            after_write();
        } else {
            log::warn!("[qbz-qt] playlist delete (from menu) failed for {id}");
            crate::toast_qt::error(qbz_i18n::t("Failed to delete playlist"));
        }
    });
}

/// Delete a playlist by ref: the local repo for a local one, the catalog
/// (delete when owned, unsubscribe otherwise) for a Qobuz one.
async fn delete_ref(id: &str, is_local: bool) -> bool {
    if is_local {
        let target = id.to_string();
        return tokio::task::spawn_blocking(move || {
            crate::local_playlist_qt::delete_blocking(&target)
        })
        .await
        .unwrap_or(false);
    }
    match id.parse::<u64>() {
        Ok(pid) => {
            let runtime = crate::app();
            match crate::playlist_qt::delete_by_id(&runtime, pid).await {
                Ok(()) => true,
                Err(e) => {
                    log::error!("[qbz-qt] {e}");
                    false
                }
            }
        }
        Err(_) => false,
    }
}

fn state() -> std::sync::MutexGuard<'static, EditState> {
    EDIT.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------------------
// Document (§4.6)
// ---------------------------------------------------------------------------

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EditDoc {
    open: bool,
    id: String,
    name: String,
    description: String,
    desc_loaded: bool,
    is_local: bool,
    offline_only: bool,
    busy: bool,
    /// The folder the playlist is filed in ("" = root), so the editor can
    /// offer "Folder" next to the name (2026-09-13).
    folder_id: String,
}

/// Serialize a snapshot. Split from [`edit_doc`] so the shape is testable
/// without touching the process-global `EDIT` — tests run in parallel threads
/// and a test that locked the global would race every other one.
fn edit_doc_of(st: EditState) -> String {
    serde_json::to_string(&EditDoc {
        open: st.open,
        id: st.id,
        name: st.name,
        description: st.description,
        desc_loaded: st.desc_loaded,
        is_local: st.is_local,
        offline_only: st.offline_only,
        busy: st.busy,
        folder_id: st.folder_id,
    })
    .unwrap_or_else(|_| "{\"open\":false}".to_string())
}

fn edit_doc() -> String {
    edit_doc_of(state().clone())
}

/// Publish the document. Also called from `boot()` so the closed FULL shape is
/// on the QML side before the first open.
pub(crate) fn publish() {
    let json = edit_doc();
    crate::playlist_edit_bridge::ui(move |mut b| {
        b.as_mut().set_edit_json(QString::from(json.as_str()));
    });
}

// ---------------------------------------------------------------------------
// The shared post-write refresh
// ---------------------------------------------------------------------------

/// Everything a successful rename or delete has to touch.
///
/// * `reload_if_loaded()` — the manager republishes, so a rename from the
///   manager updates the card instead of leaving the old name on it until the
///   view is re-entered (the reference never refreshes its own model here).
///   It is the ONE call allowed to early-return on a cold cache, so driving
///   the editor from the sidebar or the detail page costs nothing.
/// * `reload_sidebar_including_local()` — the OFFLINE-SAFE sidebar verb.
///   `reload_sidebar()` bails while offline, which would make renaming or
///   deleting a LOCAL playlist change nothing on screen for exactly the
///   population local playlists exist for.
fn after_write() {
    crate::playlist_manager_qt::reload_if_loaded();
    crate::reload_sidebar_including_local();
}

// ---------------------------------------------------------------------------
// Open
// ---------------------------------------------------------------------------

/// The resolved seeds for one playlist.
struct Seed {
    name: String,
    description: String,
    desc_loaded: bool,
    is_local: bool,
    offline_only: bool,
    folder_id: String,
}

/// The folder a playlist is filed in, read off the same tables the sidebar
/// reads: `local_playlists.folder_id` for a local one, the per-playlist
/// settings for a Qobuz one. Blocking (SQLite); call off the UI thread.
fn folder_of_blocking(id: &str) -> String {
    match crate::local_playlist_qt::PlaylistRef::parse(id) {
        Some(crate::local_playlist_qt::PlaylistRef::Local(local)) => {
            crate::local_playlist_qt::get_blocking(&local)
                .and_then(|p| p.folder_id)
                .unwrap_or_default()
        }
        Some(crate::local_playlist_qt::PlaylistRef::Qobuz(pid)) => {
            crate::folders_qt::playlist_settings_map()
                .get(&pid)
                .and_then(|s| s.folder_id.clone())
                .unwrap_or_default()
        }
        None => String::new(),
    }
}

/// Open the editor on `id`.
///
/// The seeds are resolved BEFORE `open` flips true (the `folder_edit_qt`
/// precedent), so the modal never renders an empty name field and then
/// repaints. §5.2's resolution order, per kind:
///
/// * `local:` → `local_playlist_qt::get_blocking` on `spawn_blocking`. A pure
///   local read: it works offline and with no Qobuz account, and it always
///   answers, so `descLoaded` is ALWAYS true on this arm.
/// * Qobuz → the manager cache when it is warm (free, and it already holds
///   the description because the fetch that filled it returned one), else
///   `core().get_playlist(pid)`.
/// * Neither could answer → `descLoaded: false`, with whatever name the
///   sidebar cache can still supply so the modal is not anonymous.
pub(crate) fn open(id: &str) {
    let id = id.trim().to_string();
    if id.is_empty() {
        return;
    }
    match crate::local_playlist_qt::PlaylistRef::parse(&id) {
        Some(crate::local_playlist_qt::PlaylistRef::Local(local)) => open_local(local),
        Some(crate::local_playlist_qt::PlaylistRef::Qobuz(pid)) => open_qobuz(id, pid),
        None => log::warn!("[qbz-qt] playlist editor: unusable id"),
    }
}

fn open_local(id: String) {
    crate::spawn(async move {
        let lookup = id.clone();
        let found =
            tokio::task::spawn_blocking(move || crate::local_playlist_qt::get_blocking(&lookup))
                .await
                .ok()
                .flatten();

        let Some(p) = found else {
            // Deleted underneath us, or the DB is unreadable. Opening an
            // editor on a playlist that no longer exists would be worse than
            // doing nothing (`folder_edit_qt::open_editor`'s precedent).
            log::warn!("[qbz-qt] playlist editor: local playlist not found");
            return;
        };
        seat(
            id,
            Seed {
                folder_id: p.folder_id.clone().unwrap_or_default(),
                name: p.name,
                description: p.description.unwrap_or_default(),
                // A local read either answered or we returned above.
                desc_loaded: true,
                is_local: true,
                offline_only: p.offline_only,
            },
        );
    });
}

fn open_qobuz(id: String, pid: u64) {
    // Warm manager cache first: it is free, it is already merged, and it
    // carries the description precisely so this lookup does not need a fetch.
    if let Some((name, description)) = crate::playlist_manager_qt::cached_playlist_seed(pid) {
        // The folder is one SQLite read; keep it off the UI thread.
        crate::spawn(async move {
            let lookup = id.clone();
            let folder_id = tokio::task::spawn_blocking(move || folder_of_blocking(&lookup))
                .await
                .unwrap_or_default();
            seat(
                id,
                Seed {
                    name,
                    description: description.unwrap_or_default(),
                    desc_loaded: true,
                    is_local: false,
                    offline_only: false,
                    folder_id,
                },
            );
        });
        return;
    }
    let runtime = crate::app();
    crate::spawn(async move {
        let lookup = id.clone();
        let folder_id = tokio::task::spawn_blocking(move || folder_of_blocking(&lookup))
            .await
            .unwrap_or_default();
        let seed = match runtime.core().get_playlist(pid).await {
            Ok(p) => Seed {
                name: p.name,
                description: p.description.unwrap_or_default(),
                desc_loaded: true,
                is_local: false,
                offline_only: false,
                folder_id: folder_id.clone(),
            },
            Err(e) => {
                // Offline, or the playlist is gone. The editor still opens —
                // deleting or unfollowing a playlist you cannot fetch is a
                // legitimate thing to want — but with the description field
                // suppressed, so an unknown description cannot be written
                // back as "" (§5.2).
                log::warn!("[qbz-qt] playlist editor: get_playlist {pid} failed: {e}");
                Seed {
                    name: crate::sidebar_qt::playlist_name(pid).unwrap_or_default(),
                    description: String::new(),
                    desc_loaded: false,
                    is_local: false,
                    offline_only: false,
                    folder_id,
                }
            }
        };
        seat(id, seed);
    });
}

/// Adopt a resolved seed as the open editor and publish it.
fn seat(id: String, seed: Seed) {
    {
        let mut st = state();
        *st = EditState {
            open: true,
            id,
            name: seed.name,
            description: seed.description,
            desc_loaded: seed.desc_loaded,
            is_local: seed.is_local,
            offline_only: seed.offline_only,
            busy: false,
            folder_id: seed.folder_id,
        };
    }
    publish();
}

// ---------------------------------------------------------------------------
// Save
// ---------------------------------------------------------------------------

/// Persist the drafts.
///
/// Validation stays deliberately weak (§5.25): the ONE gate is a non-blank
/// name, and the modal's Save button reads the same gate so a whitespace-only
/// name shows as disabled instead of lying.
pub(crate) fn save(name: &str, description: &str, offline_only: bool) {
    let trimmed = name.trim().to_string();
    let (id, is_local, desc_loaded) = {
        let mut st = state();
        if !st.open || st.busy || st.id.is_empty() || trimmed.is_empty() {
            return;
        }
        st.busy = true;
        (st.id.clone(), st.is_local, st.desc_loaded)
    };
    publish();

    // `None` = leave the stored description alone. Only a description the user
    // actually SAW may be asserted (§5.2).
    let desc = if desc_loaded {
        Some(description.trim().to_string())
    } else {
        None
    };

    crate::spawn(async move {
        let ok = if is_local {
            save_local(&id, &trimmed, desc, offline_only).await
        } else {
            save_qobuz(&id, &trimmed, desc).await
        };
        settle(ok, "Failed to rename playlist", "rename");
    });
}

// ---------------------------------------------------------------------------
// Cover actions
// ---------------------------------------------------------------------------

/// Open the native cover picker for the playlist currently held by the
/// editor. QML deliberately supplies no id: a stale modal document must not
/// be able to redirect a cover write to another playlist.
pub(crate) fn choose_cover() {
    let id = {
        let st = state();
        if !st.open || st.busy || st.id.is_empty() {
            return;
        }
        st.id.clone()
    };
    crate::cover_artwork_qt::add_custom_playlist_cover(id);
}

/// Remove the custom cover from the playlist currently held by the editor.
/// This is harmless when no override exists and also clears the legacy local
/// playlist artwork column through `cover_artwork_qt`.
pub(crate) fn remove_cover() {
    let id = {
        let st = state();
        if !st.open || st.busy || st.id.is_empty() {
            return;
        }
        st.id.clone()
    };
    crate::cover_artwork_qt::remove_custom_playlist_cover(id);
}

async fn save_local(id: &str, name: &str, desc: Option<String>, offline_only: bool) -> bool {
    let id = id.to_string();
    let name = name.to_string();
    tokio::task::spawn_blocking(move || {
        // `update_blocking` always writes the description column, and a `None`
        // there is SQL NULL — so when the description was never resolved the
        // STORED one is re-read and written back unchanged rather than wiped.
        // (`descLoaded` is always true on the local arm today; this is the
        // belt that keeps a future caller from silently deleting data.)
        let description = match desc {
            Some(d) => Some(d),
            None => crate::local_playlist_qt::get_blocking(&id).and_then(|p| p.description),
        };
        crate::local_playlist_qt::update_blocking(&id, &name, description.as_deref(), offline_only)
    })
    .await
    .unwrap_or(false)
}

async fn save_qobuz(id: &str, name: &str, desc: Option<String>) -> bool {
    let Ok(pid) = id.parse::<u64>() else {
        return false;
    };
    let runtime = crate::app();
    // `offline_only` is a `local_playlists` column and has no Qobuz analogue —
    // the modal only renders that row when `isLocal`, so nothing is dropped.
    match crate::playlist_qt::rename_by_id(&runtime, pid, name, desc.as_deref()).await {
        Ok(()) => true,
        Err(e) => {
            log::error!("[qbz-qt] {e}");
            false
        }
    }
}

// ---------------------------------------------------------------------------
// Delete
// ---------------------------------------------------------------------------

/// Delete the open playlist.
///
/// There is no confirm sub-modal, matching the reference (`EditPlaylistModal`
/// calls `EditPlaylistActions.delete()` straight from its danger button) —
/// unlike the FOLDER editor, whose reference raises a native rfd message box
/// that this port replaced with `QbzConfirmModal`.
pub(crate) fn delete_playlist() {
    let (id, is_local) = {
        let mut st = state();
        if !st.open || st.busy || st.id.is_empty() {
            return;
        }
        st.busy = true;
        (st.id.clone(), st.is_local)
    };
    publish();

    crate::spawn(async move {
        let ok = delete_ref(&id, is_local).await;

        if ok {
            // CONDITIONAL (§5.1): only when the user is actually standing on
            // that playlist's detail page. Deleting from the manager or the
            // sidebar must leave them where they are.
            crate::playlist_qt::back_if_showing(&id);
        }
        settle(ok, "Failed to delete playlist", "delete");
    });
}

// ---------------------------------------------------------------------------
// Close + the shared outcome
// ---------------------------------------------------------------------------

/// Clear `busy`, close on success only (D22), publish, then refresh or toast.
fn settle(ok: bool, msgid: &'static str, verb: &'static str) {
    {
        let mut st = state();
        st.busy = false;
        // Stay OPEN on failure, with everything the user typed intact.
        st.open = !ok;
    }
    publish();

    if ok {
        after_write();
    } else {
        log::warn!("[qbz-qt] playlist {verb} failed");
        crate::toast_qt::error(qbz_i18n::t(msgid));
    }
}

/// Dismiss. Refused while a save or delete is in flight (D22) — the modal is
/// the only thing reporting that a write is running.
pub(crate) fn close() {
    {
        let mut st = state();
        if st.busy {
            return;
        }
        st.open = false;
    }
    publish();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_closed_document_parses_and_carries_the_full_shape() {
        // Frame-1 safety: every key the modal reads must be present, or a
        // binding on the pre-publish frame reads `undefined`.
        let json = edit_doc_of(EditState::default());
        let v: serde_json::Value = serde_json::from_str(&json).expect("edit doc parses");
        for key in [
            "open",
            "id",
            "name",
            "description",
            "descLoaded",
            "isLocal",
            "offlineOnly",
            "busy",
        ] {
            assert!(v.get(key).is_some(), "missing key {key}");
        }
        assert_eq!(v["open"], serde_json::Value::Bool(false));
    }

    #[test]
    fn a_local_id_stays_a_string_and_camel_cases_the_flags() {
        // Nothing in the pipeline may parse `local:<uuid>` as an integer
        // (§3.1), and QML reads `isLocal` / `offlineOnly` / `descLoaded` —
        // a snake_case key here would read `undefined` and the offline-only
        // row would silently never render.
        let json = edit_doc_of(EditState {
            open: true,
            id: "local:abc-def".into(),
            name: "Road trip".into(),
            description: "sunny".into(),
            desc_loaded: true,
            is_local: true,
            offline_only: true,
            busy: false,
            folder_id: "folder-1".into(),
        });
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["id"], serde_json::Value::String("local:abc-def".into()));
        assert_eq!(v["isLocal"], serde_json::Value::Bool(true));
        assert_eq!(v["folderId"], serde_json::Value::String("folder-1".into()));
        assert_eq!(v["offlineOnly"], serde_json::Value::Bool(true));
        assert_eq!(v["descLoaded"], serde_json::Value::Bool(true));
        assert!(v.get("is_local").is_none());
    }

    #[test]
    fn an_unresolved_description_is_published_as_not_loaded() {
        // The modal keys the whole description field off this flag, and `save`
        // keys `None` off it — the one thing standing between a failed fetch
        // and a wiped description (§5.2).
        let json = edit_doc_of(EditState {
            open: true,
            id: "42".into(),
            name: "Jazz".into(),
            desc_loaded: false,
            ..EditState::default()
        });
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["descLoaded"], serde_json::Value::Bool(false));
        assert_eq!(v["description"], serde_json::Value::String(String::new()));
    }
}
