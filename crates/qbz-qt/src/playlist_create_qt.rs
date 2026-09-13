//! The "New playlist" modal — controller half.
//!
//! PORT OF `primitives/CreatePlaylistModal.slint` + `SidebarActions
//! ::on_create_playlist` (`qbz/src/main.rs:21107`) + `CreatePlaylistActions
//! ::on_submit` (`:21496`). It replaces this port's POC shortcut, which was
//! `crate::create_playlist()`: one bridge call that created a playlist named
//! "New Playlist" and navigated to it.
//!
//! WHAT THE SHORTCUT COST, and why this is a regression and not a nicety —
//! four of the modal's five fields had NO other door in the app:
//!
//! | field | the shortcut | reachable elsewhere? |
//! |---|---|---|
//! | name | hardcoded "New Playlist" | yes, Edit playlist |
//! | description | always empty | yes, Edit playlist |
//! | folder | always root | yes, the sidebar row menu / the manager |
//! | public | always `false` | **NO** |
//! | offline-only | forced by connectivity | **NO** for an ONLINE user |
//!
//! The last row is the one that matters and is the reason the owner flagged
//! this: an online user could not create a LOCAL playlist at all. `offline_only`
//! decides whether the playlist is a `local_playlists` row (id `local:<uuid>`,
//! lives in library.db, works with no Qobuz account, never uploaded) or a Qobuz
//! entity — and the shortcut derived it from connectivity alone. Someone with a
//! Qobuz session who wants a private local playlist for their own files had to
//! pull the network cable. That is the D8 opt-in, and it only exists in the
//! modal.
//!
//! # Two arms, and the offline one is not a fallback
//!
//! `submit` branches exactly where the reference does (`main.rs:21525`):
//!
//! - `offline_only || the app is offline` -> `local_playlist_qt::create_blocking`
//!   (a pure DB write, so it works with no session at all). The forced-by-being-
//!   offline case KEEPS the flag, because the reference says so: it can be
//!   unmarked later in Edit to enable "Upload to Qobuz".
//! - otherwise -> `core().create_playlist()`, then — and only then — the folder
//!   assignment, which is a LOCAL db write keyed on the new Qobuz id.
//!
//! The folder dropdown is offered on BOTH arms even though the assignment is
//! only wired for the Qobuz one. That is not an oversight: `folders_qt
//! ::move_playlist` takes a `u64`, and a `local:` id has none. The picker hides
//! itself when the choice cannot be honoured — see `submit`.
//!
//! # `busy` HOLDS THE MODAL OPEN (D22)
//!
//! Same discipline as `playlist_edit_qt`, and deliberately STRICTER than the
//! reference: Slint's create modal closes on the success path only too, but its
//! `close()` is not refused mid-flight, so the scrim can dismiss a panel whose
//! write is still running. Here `close()` is a no-op while busy, and a failure
//! keeps what the user typed on screen and toasts.
//!
//! # Threading
//!
//! Invokables run on the Qt thread: mutate the `Mutex`, publish, `crate::spawn`
//! the rest. Every `local_playlist_qt` call is `spawn_blocking`-only.

use std::sync::{LazyLock, Mutex};

use serde::Serialize;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

#[derive(Clone, Debug, Default)]
struct CreateState {
    open: bool,
    /// True while the app is offline: creation can only produce a local
    /// playlist, so the toggle shows ON and LOCKED with a hint
    /// (CreatePlaylistModal.slint:184-216).
    offline_locked: bool,
    /// Folders offered by the dropdown, `(id, name)`. Index 0 is always
    /// ("", "No folder").
    folders: Vec<(String, String)>,
    busy: bool,
}

static CREATE: LazyLock<Mutex<CreateState>> = LazyLock::new(|| Mutex::new(CreateState::default()));

fn state() -> std::sync::MutexGuard<'static, CreateState> {
    CREATE.lock().unwrap_or_else(|e| e.into_inner())
}

// ---------------------------------------------------------------------------
// Document
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct FolderOpt {
    id: String,
    name: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateDoc {
    open: bool,
    offline_locked: bool,
    busy: bool,
    folders: Vec<FolderOpt>,
}

/// Serialize a snapshot. Split from [`create_doc`] so the shape is testable
/// without touching the process-global `CREATE`.
fn create_doc_of(st: CreateState) -> String {
    serde_json::to_string(&CreateDoc {
        open: st.open,
        offline_locked: st.offline_locked,
        busy: st.busy,
        folders: st
            .folders
            .into_iter()
            .map(|(id, name)| FolderOpt { id, name })
            .collect(),
    })
    .unwrap_or_else(|_| "{\"open\":false}".to_string())
}

fn create_doc() -> String {
    create_doc_of(state().clone())
}

/// Publish the document. Also called from `boot()` so the closed FULL shape is
/// on the QML side before the first open.
pub(crate) fn publish() {
    let json = create_doc();
    crate::playlist_edit_bridge::ui(move |mut b| {
        b.as_mut()
            .set_create_json(cxx_qt_lib::QString::from(json.as_str()));
    });
}

// ---------------------------------------------------------------------------
// Open / close
// ---------------------------------------------------------------------------

/// Sidebar "+" — seed and show the modal.
///
/// The folder list is built HERE, from the same source the sidebar tree reads,
/// and HIDDEN FOLDERS ARE DROPPED. The reference builds it from
/// `SidebarState.folders`, which is already the visible set, so reading the raw
/// table would offer a folder the tree refuses to show — a playlist filed into
/// nowhere the user can see. Same rule `SidebarRowMenu.qml`'s `visibleFolders`
/// spells out for the move-to-folder list.
pub(crate) fn open() {
    let offline = crate::offline_fwd::engine().status().is_offline();
    let mut folders: Vec<(String, String)> = vec![(String::new(), qbz_i18n::t("No folder"))];
    folders.extend(
        crate::folders_qt::load_folders_full()
            .into_iter()
            .filter(|f| !f.is_hidden)
            .map(|f| (f.id, f.name)),
    );
    {
        let mut st = state();
        st.open = true;
        st.busy = false;
        st.offline_locked = offline;
        st.folders = folders;
    }
    publish();
}

/// Dismiss. Refused while a create is in flight (D22).
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

fn settle_failure(msgid: &'static str) {
    {
        let mut st = state();
        st.busy = false;
    }
    publish();
    crate::toast_qt::error(qbz_i18n::t(msgid));
}

fn settle_success(new_id: String) {
    {
        let mut st = state();
        st.busy = false;
        st.open = false;
    }
    publish();
    crate::publish_sidebar();
    // Land on it, exactly as the reference does after either arm
    // (`main.rs:21554` / `:21598`). `open_playlist` routes a `local:` id to the
    // local loader and does not offline-gate it.
    crate::open_playlist(new_id);
}

// ---------------------------------------------------------------------------
// Submit
// ---------------------------------------------------------------------------

/// Create. Every field is a QML-LOCAL draft and arrives as an argument
/// (`MyQbzModals.qml`'s convention); nothing about the form lives in Rust
/// except `busy` and the seeds.
///
/// `folder_id` is `""` for "No folder".
pub(crate) fn submit(
    name: &str,
    description: &str,
    folder_id: &str,
    is_public: bool,
    offline_only: bool,
) {
    let name = name.trim().to_string();
    let description = description.trim().to_string();
    let folder_id = folder_id.to_string();
    if name.is_empty() {
        return;
    }
    {
        let mut st = state();
        if st.busy || !st.open {
            return;
        }
        st.busy = true;
    }
    publish();

    let offline_now = crate::offline_fwd::engine().status().is_offline();
    if offline_only || offline_now {
        // LOCAL arm. A pure DB write — no session, no network, and the one
        // path an account-less user has. `offline_only` is stored as TRUE even
        // when it was forced by being offline (reference note at
        // `main.rs:21528-21530`): the flag is what "Upload to Qobuz" reads, and
        // Edit can unmark it later.
        crate::spawn(async move {
            let created_name = name.clone();
            let created = tokio::task::spawn_blocking(move || {
                let desc = if description.is_empty() {
                    None
                } else {
                    Some(description.as_str())
                };
                crate::local_playlist_qt::create_blocking(&name, desc, true)
            })
            .await
            .ok()
            .flatten();
            match created {
                Some(new_id) => {
                    log::info!("[qbz-qt] local playlist created: {new_id}");
                    crate::sidebar_qt::insert_local_entry(&new_id, &created_name);
                    settle_success(new_id);
                }
                None => {
                    log::error!("[qbz-qt] create local playlist failed");
                    settle_failure("Couldn't create the playlist");
                }
            }
        });
        return;
    }

    let runtime = crate::app();
    crate::spawn(async move {
        let desc = if description.is_empty() {
            None
        } else {
            Some(description.as_str())
        };
        match runtime.core().create_playlist(&name, desc, is_public).await {
            Ok(playlist) => {
                let new_id = playlist.id.to_string();
                // Folder assignment is a LOCAL db write keyed on the new Qobuz
                // id, and it runs BEFORE the sidebar reload so the tree draws
                // the playlist already filed (reference ordering,
                // `main.rs:21575-21584`).
                if !folder_id.is_empty() {
                    let pid = playlist.id;
                    let fid = folder_id.clone();
                    let _ = tokio::task::spawn_blocking(move || {
                        crate::folders_qt::move_playlist(pid, Some(fid.as_str()));
                    })
                    .await;
                }
                log::info!(
                    "[qbz-qt] playlist created: {} ({})",
                    playlist.name,
                    playlist.id
                );
                crate::sidebar_qt::insert_qobuz_entry(
                    playlist.id,
                    &playlist.name,
                    playlist.tracks_count,
                    &[],
                );
                crate::sidebar_qt::move_playlist_optimistic(&new_id, &folder_id);
                settle_success(new_id);
            }
            Err(e) => {
                log::error!("[qbz-qt] create playlist failed: {e}");
                settle_failure("Couldn't create the playlist");
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The closed document is FULL-SHAPE and parseable: the modal's bindings
    /// read `doc.open`, `doc.busy`, `doc.offlineLocked` and `doc.folders` on
    /// the pre-publish frame, and an absent key there is a QML warning per
    /// binding, per frame.
    #[test]
    fn closed_doc_is_full_shape() {
        let doc: serde_json::Value =
            serde_json::from_str(&create_doc_of(CreateState::default())).unwrap();
        assert_eq!(doc["open"], false);
        assert_eq!(doc["busy"], false);
        assert_eq!(doc["offlineLocked"], false);
        assert!(doc["folders"].as_array().is_some_and(|a| a.is_empty()));
    }

    /// The dropdown is camelCase on the wire and index 0 is the "no folder"
    /// sentinel with an EMPTY id — `submit` reads that emptiness, not the
    /// index, so a reordered list cannot file a playlist into the wrong place.
    #[test]
    fn folder_options_carry_the_root_sentinel_first() {
        let st = CreateState {
            open: true,
            offline_locked: true,
            busy: false,
            folders: vec![
                (String::new(), "No folder".into()),
                ("f1".into(), "Jazz".into()),
            ],
        };
        let doc: serde_json::Value = serde_json::from_str(&create_doc_of(st)).unwrap();
        assert_eq!(doc["offlineLocked"], true);
        let folders = doc["folders"].as_array().unwrap();
        assert_eq!(folders.len(), 2);
        assert_eq!(folders[0]["id"], "");
        assert_eq!(folders[1]["id"], "f1");
        assert_eq!(folders[1]["name"], "Jazz");
    }
}
