//! Folders tab, TREE mode: the flattened lazy folder tree (expanded set +
//! one-level fetches, flattened to a windowable array), the rail's
//! multi-select state, and the right-hand folder-detail pane.
//!
//! Split out of `local_library_qt.rs` (phase-24 modularization). Local-only
//! by nature — Plex rows have no filesystem folder, so nothing here is
//! Plex-aware.
//!
//! The selection lives HERE, next to the tree it annotates: a tree row's
//! `selected` / `selectState` is derived at publish time from the selected
//! set, so the array and the selection can never drift. The Qt-facing half
//! (publish + the bulk actions) is `local_bulk.rs` — this file stays free of
//! cxx-qt so it can run on a blocking thread.

use std::collections::HashMap;
use std::sync::Mutex;

use qbz_library::{FolderTreeEntry, LibraryError, LocalTrack};
use serde::Serialize;

use qbz_source::SourceId;

use crate::local_rows::{
    art_token, basename, folder_key, map_track, FolderDetail, SubfolderRow, TrackRow, TreeNode,
};
use crate::local_state::{state, with_art, with_db};

/// `list_folder_children` + the reference's on-disk cover fallback for the
/// Folder entries (`local_library.rs:2947-2966`): the index carries an
/// `artwork` only when a track under the folder had embedded art, so a folder
/// whose cover sits on disk as `cover.jpg` / `folder.jpg` / `<album>.jpg` drew
/// a blank subcard here while the Slint drew the cover. Same
/// `find_folder_cover` the queue backfill uses, so the tree card and the queue
/// thumbnail can never disagree.
///
/// BLOCKING (one `read_dir` per child folder that has no indexed artwork) —
/// both callers are `*_blocking` and run inside `spawn_blocking`, which is
/// where the reference puts it too ("Off-thread, so the fs scan is fine here").
fn folder_children_with_covers(path: &str) -> Vec<FolderTreeEntry> {
    with_db(|db| db.list_folder_children(path, false))
        .unwrap_or_default()
        .into_iter()
        .map(|e| match e {
            FolderTreeEntry::Folder {
                path,
                segment,
                track_count_under,
                artwork,
            } => {
                let artwork = artwork.filter(|a| !a.is_empty()).or_else(|| {
                    crate::local_playback::find_folder_cover(std::path::Path::new(&path))
                });
                FolderTreeEntry::Folder {
                    path,
                    segment,
                    track_count_under,
                    artwork,
                }
            }
            other => other,
        })
        .collect()
}

fn entry_to_node(
    e: &FolderTreeEntry,
    depth: i32,
    art: &mut std::collections::HashMap<String, (SourceId, String)>,
) -> TreeNode {
    match e {
        FolderTreeEntry::Folder {
            path,
            segment,
            track_count_under,
            artwork,
        } => {
            let key = folder_key(path);
            if let Some(a) = artwork.as_ref().filter(|a| !a.is_empty()) {
                // The folder tree is `library.db`: every cover here is a
                // local file.
                if let Some(t) = art_token(Some("local"), a) {
                    art.insert(key.clone(), t);
                }
            }
            TreeNode {
                path: path.clone(),
                segment: segment.clone(),
                depth,
                is_folder: true,
                can_expand: *track_count_under > 0,
                expanded: false,
                track_count: *track_count_under,
                art_key: key,
            }
        }
        FolderTreeEntry::Track { path, segment } => TreeNode {
            path: path.clone(),
            segment: segment.clone(),
            depth,
            is_folder: false,
            can_expand: false,
            expanded: false,
            track_count: 0,
            art_key: String::new(),
        },
    }
}

/// Seed the tree with the registered library folders (depth 0).
pub fn load_tree_roots_blocking() -> Vec<TreeNode> {
    let roots = with_db(|db| {
        let paths = db.get_folders()?;
        let mut out = Vec::with_capacity(paths.len());
        for p in paths {
            let cnt = db.count_folder_tracks_recursive(&p, false)?;
            out.push((p, cnt));
        }
        Ok::<_, LibraryError>(out)
    })
    .unwrap_or_default();
    let nodes: Vec<TreeNode> = roots
        .into_iter()
        .map(|(p, cnt)| TreeNode {
            art_key: folder_key(&p),
            segment: basename(&p),
            path: p,
            depth: 0,
            is_folder: true,
            can_expand: cnt > 0,
            expanded: false,
            track_count: cnt,
        })
        .collect();
    state(|s| s.tree = nodes.clone());
    nodes
}

/// Collapse: drop the contiguous descendant block (pure UI, no query).
pub fn tree_collapse(path: &str) -> Vec<TreeNode> {
    state(|s| {
        if let Some(pos) = s.tree.iter().position(|n| n.path == path) {
            let depth = s.tree[pos].depth;
            s.tree[pos].expanded = false;
            let mut end = pos + 1;
            while end < s.tree.len() && s.tree[end].depth > depth {
                end += 1;
            }
            s.tree.drain(pos + 1..end);
        }
        s.tree.clone()
    })
}

/// Expand: fetch ONE child level and splice it in (never a recursive
/// preload).
pub fn tree_expand_blocking(path: &str) -> Vec<TreeNode> {
    let children = folder_children_with_covers(path);
    let nodes = state(|s| {
        s.tree
            .iter()
            .position(|n| n.path == path)
            .map(|pos| (pos, s.tree[pos].depth))
    });
    let Some((pos, depth)) = nodes else {
        return state(|s| s.tree.clone());
    };
    let mapped = with_art(|art| {
        children
            .iter()
            .map(|e| entry_to_node(e, depth + 1, art))
            .collect::<Vec<TreeNode>>()
    });
    state(|s| {
        s.tree[pos].expanded = true;
        for (i, n) in mapped.into_iter().enumerate() {
            s.tree.insert(pos + 1 + i, n);
        }
        s.tree.clone()
    })
}

pub fn tree_collapse_all() -> Vec<TreeNode> {
    state(|s| {
        s.tree.retain(|n| n.depth == 0);
        for n in s.tree.iter_mut() {
            n.expanded = false;
        }
        s.tree.clone()
    })
}

pub fn set_tree_search(query: &str) {
    state(|s| s.tree_search = query.trim().to_lowercase());
}

/// The rail's VISIBLE set: the flattened tree filtered by the rail search,
/// keeping every match AND its ancestors so the tree stays navigable.
fn visible_nodes() -> Vec<TreeNode> {
    state(|s| {
        if s.tree_search.is_empty() {
            return s.tree.clone();
        }
        let q = &s.tree_search;
        let matches: Vec<String> = s
            .tree
            .iter()
            .filter(|n| n.segment.to_lowercase().contains(q))
            .map(|n| n.path.clone())
            .collect();
        s.tree
            .iter()
            .filter(|n| {
                n.segment.to_lowercase().contains(q)
                    || matches
                        .iter()
                        .any(|m| m.starts_with(&format!("{}/", n.path)))
            })
            .cloned()
            .collect()
    })
}

/// What the bridge publishes: the visible set, annotated with the current
/// selection.
pub fn tree_visible() -> Vec<TreeNodeOut> {
    annotate(visible_nodes())
}

// ---------------------------------------------------------------------------
// Rail multi-select (LocalLibraryView.slint:1779 bulk bar; the Slint's
// `TREE_SELECTED` map + `apply_tree` annotation, 1:1)
// ---------------------------------------------------------------------------

/// A tree row AS PUBLISHED: the node plus its selection annotation. The
/// annotation is DERIVED per publish and never stored on the node, so the
/// tree array and the selected set cannot disagree.
#[derive(Serialize)]
pub struct TreeNodeOut {
    #[serde(flatten)]
    pub node: TreeNode,
    /// Track rows only (folders use `selectState`).
    pub selected: bool,
    /// Folder rows only: 0 none / 1 partial / 2 all — the QML `SelectCheck`
    /// tri-state.
    #[serde(rename = "selectState")]
    pub select_state: i32,
}

/// The selected TRACK records, keyed by file path. The key is exactly the
/// `path` a track node carries, which is what lets a folder's tri-state be a
/// prefix RANGE instead of a per-folder recursive query.
///
/// Select MODE itself is not mirrored here: QML owns the flag (it drives
/// nothing but chrome) and tells us only when it turns OFF, which is when the
/// selection must be dropped.
static TREE_SEL: Mutex<Option<HashMap<String, LocalTrack>>> = Mutex::new(None);

fn sel<R>(f: impl FnOnce(&mut HashMap<String, LocalTrack>) -> R) -> R {
    let mut guard = TREE_SEL.lock().unwrap_or_else(|e| e.into_inner());
    f(guard.get_or_insert_with(HashMap::new))
}

/// Annotate the visible rows. ONE sorted view of the selected paths is built
/// per publish; each folder's "how many of my tracks are selected" is then a
/// binary-searched range and each track row a binary search — instead of the
/// naive per-folder scan of the whole selection (a 16K-track "check all"
/// against a few hundred open rows is millions of prefix compares).
fn annotate(nodes: Vec<TreeNode>) -> Vec<TreeNodeOut> {
    let mut paths: Vec<String> = sel(|s| s.keys().cloned().collect());
    if paths.is_empty() {
        return nodes
            .into_iter()
            .map(|node| TreeNodeOut {
                node,
                selected: false,
                select_state: 0,
            })
            .collect();
    }
    paths.sort_unstable();
    nodes
        .into_iter()
        .map(|node| {
            let (selected, select_state) = if node.is_folder {
                // Byte-order range over "<path>/…": the upper bound is the
                // same prefix with its trailing '/' (0x2F) bumped to '0'
                // (0x30), which is the next byte value.
                let lo = format!("{}/", node.path);
                let hi = format!("{}0", node.path);
                let start = paths.partition_point(|p| p.as_str() < lo.as_str());
                let end = paths.partition_point(|p| p.as_str() < hi.as_str());
                let under = (end - start) as u32;
                let st = if under == 0 {
                    0
                } else if node.track_count > 0 && under >= node.track_count {
                    2
                } else {
                    1
                };
                (false, st)
            } else {
                (paths.binary_search(&node.path).is_ok(), 0)
            };
            TreeNodeOut {
                node,
                selected,
                select_state,
            }
        })
        .collect()
}

pub fn tree_selected_count() -> i32 {
    sel(|s| s.len() as i32)
}

/// Rail header toggle. LEAVING select mode drops the selection (Slint
/// `toggle_tree_select_mode`) — re-entering always starts clean.
pub fn set_tree_select_mode(on: bool) {
    if !on {
        tree_clear_selection();
    }
}

/// Drop the selection. Also the logout / user-switch hook — the records
/// belong to the previous user's `library.db`, so `local_library_qt::reset`
/// must call this next to `local_state::reset`.
pub fn tree_clear_selection() {
    sel(|s| s.clear());
}

/// The selected rows in scan (path) order — the order a bulk enqueue plays.
pub fn tree_selected_snapshot() -> Vec<LocalTrack> {
    let mut rows: Vec<LocalTrack> = sel(|s| s.values().cloned().collect());
    rows.sort_by(|a, b| a.file_path.cmp(&b.file_path));
    rows
}

/// Folder checkbox: toggle every track UNDER it, recursively. Already all
/// selected -> deselect them; otherwise select them all.
pub fn toggle_folder_select_blocking(path: &str) {
    let tracks = with_db(|db| db.list_folder_tracks_recursive(path, false)).unwrap_or_default();
    if tracks.is_empty() {
        return;
    }
    sel(|s| {
        let all = tracks.iter().all(|t| s.contains_key(&t.file_path));
        if all {
            for t in &tracks {
                s.remove(&t.file_path);
            }
        } else {
            for t in tracks {
                s.insert(t.file_path.clone(), t);
            }
        }
    });
}

/// Track checkbox. Deselect is a map removal; SELECT has to resolve the
/// record, and the tree only carries paths — so the parent folder's direct
/// listing is the (single, cheap) query that yields it.
pub fn toggle_track_select_blocking(path: &str) {
    if sel(|s| s.remove(path)).is_some() {
        return;
    }
    if let Some(track) = resolve_track_record(path) {
        sel(|s| {
            s.insert(path.to_string(), track);
        });
    }
}

/// The library record behind a track leaf's path.
///
/// SACD leaves deliberately carry their authoritative virtual
/// `sacd:<image>#N` path, whose filesystem parent is not the synthetic ISO
/// folder shown in the tree. Resolve the exact key first; ordinary files take
/// this same cheap indexed path and no longer need a parent listing.
fn resolve_track_record(path: &str) -> Option<LocalTrack> {
    if let Some(track) = with_db(|db| db.get_track_by_path(path)).flatten() {
        return Some(track);
    }
    let parent = std::path::Path::new(path)
        .parent()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tracks = with_db(|db| db.list_folder_tracks(&parent, false)).unwrap_or_default();
    tracks.into_iter().find(|t| t.file_path == path)
}

/// Shift-click in the rail: SELECT — never toggle — every node the range
/// covers (`nodes` = `(path, is_folder)` of the visible rows from the anchor
/// to the clicked row). A folder brings its whole subtree and a track its own
/// record. Inserting is idempotent, so a folder and its expanded children
/// sharing the range, or rows that were already ticked, simply stay ticked:
/// a range only ever adds, like every other select-mode surface.
pub fn select_nodes_blocking(nodes: &[(String, bool)]) {
    for (path, is_folder) in nodes {
        if *is_folder {
            let tracks =
                with_db(|db| db.list_folder_tracks_recursive(path, false)).unwrap_or_default();
            sel(|s| {
                for t in tracks {
                    s.insert(t.file_path.clone(), t);
                }
            });
        } else if !sel(|s| s.contains_key(path.as_str())) {
            if let Some(track) = resolve_track_record(path) {
                sel(|s| {
                    s.insert(path.clone(), track);
                });
            }
        }
    }
}

/// Bulk-bar "Check all" — TWO-WAY, like the Slint: when everything under the
/// registered roots is already selected the button un-selects it all.
pub fn tree_select_all_blocking() {
    let roots = with_db(|db| db.get_folders()).unwrap_or_default();
    let mut all: Vec<LocalTrack> = Vec::new();
    for p in roots {
        if let Some(mut rows) = with_db(|db| db.list_folder_tracks_recursive(&p, false)) {
            all.append(&mut rows);
        }
    }
    if all.is_empty() {
        return;
    }
    sel(|s| {
        let every = all.iter().all(|t| s.contains_key(&t.file_path));
        if every {
            for t in &all {
                s.remove(&t.file_path);
            }
        } else {
            for t in all {
                s.insert(t.file_path.clone(), t);
            }
        }
    });
}

/// The right pane of tree mode: subfolders (cover cards) + the folder's
/// DIRECT tracks + the recursive count shown next to the name.
pub fn load_folder_detail_blocking(path: &str) -> FolderDetail {
    let children = folder_children_with_covers(path);
    let tracks = with_db(|db| db.list_folder_tracks(path, false)).unwrap_or_default();
    let count = with_db(|db| db.count_folder_tracks_recursive(path, false)).unwrap_or(0);
    let (subfolders, rows) = with_art(|art| {
        let subfolders: Vec<SubfolderRow> = children
            .iter()
            .filter_map(|e| match e {
                FolderTreeEntry::Folder {
                    path,
                    segment,
                    track_count_under,
                    artwork,
                } => {
                    let key = folder_key(path);
                    if let Some(a) = artwork.as_ref().filter(|a| !a.is_empty()) {
                        // The folder tree is `library.db`: every cover here is a
                        // local file.
                        if let Some(t) = art_token(Some("local"), a) {
                            art.insert(key.clone(), t);
                        }
                    }
                    Some(SubfolderRow {
                        path: path.clone(),
                        name: segment.clone(),
                        track_count: *track_count_under,
                        art_key: key,
                    })
                }
                FolderTreeEntry::Track { .. } => None,
            })
            .collect();
        let rows: Vec<TrackRow> = tracks.iter().map(|t| map_track(t, art)).collect();
        (subfolders, rows)
    });
    // Raw rows for context-menu enqueue (see `local_playback`).
    state(|s| s.detail_raw = tracks.clone());
    FolderDetail {
        path: path.to_string(),
        name: basename(path),
        track_count: count,
        subfolders,
        tracks: rows,
    }
}
