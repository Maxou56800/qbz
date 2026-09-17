//! The delta: what the target account is missing, given the source
//! snapshot, the target's live state and the ledger. Pure — no I/O — so
//! the rules are unit-tested here and the applier only executes.

use std::collections::{HashMap, HashSet};

use serde::Serialize;

use crate::ledger::Ledger;
use crate::snapshot::{AccountSnapshot, FavoriteItem, FavoriteKind, OwnedPlaylist};

/// One playlist the applier will act on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum PlaylistAction {
    /// No playlist of that name on the target: create it and fill it.
    Create {
        source_id: u64,
        name: String,
        description: Option<String>,
        is_public: bool,
        track_ids: Vec<u64>,
    },
    /// A target playlist of the same name holds nothing the source lacks:
    /// it is the same playlist. Append what it is missing, in source order.
    Merge {
        source_id: u64,
        target_id: u64,
        name: String,
        missing_track_ids: Vec<u64>,
    },
    /// A target playlist of the same name holds tracks the source does not:
    /// a different playlist. Create a copy with a suffix rather than mix.
    CreateCopy {
        source_id: u64,
        name: String,
        description: Option<String>,
        is_public: bool,
        track_ids: Vec<u64>,
    },
}

impl PlaylistAction {
    pub fn source_id(&self) -> u64 {
        match self {
            PlaylistAction::Create { source_id, .. }
            | PlaylistAction::Merge { source_id, .. }
            | PlaylistAction::CreateCopy { source_id, .. } => *source_id,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            PlaylistAction::Create { name, .. }
            | PlaylistAction::Merge { name, .. }
            | PlaylistAction::CreateCopy { name, .. } => name,
        }
    }

    /// Tracks this action will write.
    pub fn track_count(&self) -> usize {
        match self {
            PlaylistAction::Create { track_ids, .. }
            | PlaylistAction::CreateCopy { track_ids, .. } => track_ids.len(),
            PlaylistAction::Merge {
                missing_track_ids, ..
            } => missing_track_ids.len(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct FavoritesPlan {
    pub kind_counts: Vec<(FavoriteKind, usize)>,
    /// Per kind, in write order (oldest favorite first).
    pub to_add: Vec<(FavoriteKind, Vec<FavoriteItem>)>,
    /// Already on the target (or in the ledger), per kind.
    pub already: Vec<(FavoriteKind, usize)>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct CloudPlan {
    pub favorites: FavoritesPlan,
    pub playlists: Vec<PlaylistAction>,
    /// Owned source playlists that need nothing (fully present already).
    pub playlists_already: usize,
    pub subscriptions: Vec<u64>,
    pub subscriptions_already: usize,
}

impl CloudPlan {
    pub fn favorites_to_add(&self) -> usize {
        self.favorites.to_add.iter().map(|(_, v)| v.len()).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.favorites_to_add() == 0 && self.playlists.is_empty() && self.subscriptions.is_empty()
    }
}

/// The suffix a copy gets when the target already has a DIFFERENT
/// playlist of the same name.
pub const COPY_SUFFIX: &str = " (migrated)";

/// Write order for favorites: oldest first, so the target's own
/// `favorited_at` stamps keep the source's relative chronology. With no
/// stamp, the server listing is newest-first, so the fallback is the
/// reverse of the listing order.
fn chronological(mut items: Vec<FavoriteItem>) -> Vec<FavoriteItem> {
    let all_stamped = items.iter().all(|f| f.favorited_at.is_some());
    if all_stamped {
        items.sort_by_key(|f| {
            (
                f.favorited_at.unwrap_or(0),
                std::cmp::Reverse(f.server_index),
            )
        });
    } else {
        items.sort_by_key(|f| std::cmp::Reverse(f.server_index));
    }
    items
}

fn normalized(name: &str) -> String {
    name.trim().to_lowercase()
}

pub fn plan(source: &AccountSnapshot, target: &AccountSnapshot, ledger: &Ledger) -> CloudPlan {
    let mut plan = CloudPlan::default();

    // --- Favorites -------------------------------------------------------
    for kind in FavoriteKind::ALL {
        let have = target.favorites.ids(kind);
        let mut add = Vec::new();
        let mut already = 0usize;
        for item in source.favorites.of(kind) {
            if have.contains(item.id.as_str()) || ledger.favorite_done(kind.plural(), &item.id) {
                already += 1;
            } else {
                add.push(item.clone());
            }
        }
        let add = chronological(add);
        plan.favorites
            .kind_counts
            .push((kind, source.favorites.of(kind).len()));
        plan.favorites.already.push((kind, already));
        plan.favorites.to_add.push((kind, add));
    }

    // --- Owned playlists ---------------------------------------------------
    let target_by_id: HashMap<u64, &OwnedPlaylist> =
        target.playlists.iter().map(|p| (p.id, p)).collect();
    let mut target_by_name: HashMap<String, Vec<&OwnedPlaylist>> = HashMap::new();
    for p in &target.playlists {
        target_by_name
            .entry(normalized(&p.name))
            .or_default()
            .push(p);
    }
    for src in &source.playlists {
        let src_set: HashSet<u64> = src.track_ids.iter().copied().collect();
        // An earlier run already mapped it: keep syncing the SAME target.
        if let Some(mapped) = ledger.playlist_map.get(&src.id) {
            if let Some(tgt) = target_by_id.get(mapped) {
                let tgt_set: HashSet<u64> = tgt.track_ids.iter().copied().collect();
                let missing: Vec<u64> = src
                    .track_ids
                    .iter()
                    .copied()
                    .filter(|id| !tgt_set.contains(id))
                    .collect();
                if missing.is_empty() {
                    plan.playlists_already += 1;
                } else {
                    plan.playlists.push(PlaylistAction::Merge {
                        source_id: src.id,
                        target_id: tgt.id,
                        name: src.name.clone(),
                        missing_track_ids: missing,
                    });
                }
                continue;
            }
            // The mapped playlist is gone from the target: fall through and
            // treat the source playlist as new.
        }
        let same_name = target_by_name
            .get(&normalized(&src.name))
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        // §9.3: same name and nothing foreign in the target copy = the same
        // playlist (possibly behind); anything foreign = a different one.
        let is_twin = |tgt: &&OwnedPlaylist| tgt.track_ids.iter().all(|id| src_set.contains(id));
        let twin = same_name.iter().copied().find(is_twin);
        // An earlier run may have created the "(migrated)" copy and died
        // before the ledger recorded it (apply.rs creates first, saves second).
        // Before creating a SECOND copy, look for that orphan by its copy
        // name under the same twin rule.
        let copy_name = format!("{}{}", src.name.trim(), COPY_SUFFIX);
        let orphan_copy = target_by_name
            .get(&normalized(&copy_name))
            .map(Vec::as_slice)
            .unwrap_or(&[])
            .iter()
            .copied()
            .find(is_twin);
        match twin.or(orphan_copy) {
            Some(tgt) => {
                let tgt_set: HashSet<u64> = tgt.track_ids.iter().copied().collect();
                let missing: Vec<u64> = src
                    .track_ids
                    .iter()
                    .copied()
                    .filter(|id| !tgt_set.contains(id))
                    .collect();
                if missing.is_empty() {
                    plan.playlists_already += 1;
                } else {
                    plan.playlists.push(PlaylistAction::Merge {
                        source_id: src.id,
                        target_id: tgt.id,
                        name: src.name.clone(),
                        missing_track_ids: missing,
                    });
                }
            }
            None if same_name.is_empty() => plan.playlists.push(PlaylistAction::Create {
                source_id: src.id,
                name: src.name.clone(),
                description: src.description.clone(),
                is_public: src.is_public,
                track_ids: src.track_ids.clone(),
            }),
            None => plan.playlists.push(PlaylistAction::CreateCopy {
                source_id: src.id,
                name: format!("{}{}", src.name.trim(), COPY_SUFFIX),
                description: src.description.clone(),
                is_public: src.is_public,
                track_ids: src.track_ids.clone(),
            }),
        }
    }

    // --- Subscriptions -----------------------------------------------------
    let target_has: HashSet<u64> = target
        .subscribed_ids()
        .union(&target.owned_ids())
        .copied()
        .collect();
    for sub in &source.subscriptions {
        if target_has.contains(&sub.id) || ledger.subscriptions_done.contains(&sub.id) {
            plan.subscriptions_already += 1;
        } else {
            plan.subscriptions.push(sub.id);
        }
    }

    plan
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::SubscribedPlaylist;

    fn fav(id: &str, at: Option<i64>, idx: usize) -> FavoriteItem {
        FavoriteItem {
            id: id.into(),
            favorited_at: at,
            title: id.into(),
            server_index: idx,
        }
    }

    fn owned(id: u64, name: &str, tracks: &[u64]) -> OwnedPlaylist {
        OwnedPlaylist {
            id,
            name: name.into(),
            description: None,
            is_public: false,
            track_ids: tracks.to_vec(),
        }
    }

    fn snap() -> AccountSnapshot {
        AccountSnapshot::empty(1, "src")
    }

    #[test]
    fn favorites_are_a_delta_written_oldest_first() {
        let mut source = snap();
        source.favorites.albums = vec![
            fav("new", Some(300), 0),
            fav("mid", Some(200), 1),
            fav("old", Some(100), 2),
            fav("have", Some(50), 3),
        ];
        let mut target = snap();
        target.favorites.albums = vec![fav("have", None, 0)];
        let p = plan(&source, &target, &Ledger::default());
        let albums = &p.favorites.to_add[0].1;
        let ids: Vec<&str> = albums.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(ids, vec!["old", "mid", "new"]);
        assert_eq!(p.favorites.already[0], (FavoriteKind::Albums, 1));
    }

    #[test]
    fn favorites_without_stamps_reverse_the_server_listing() {
        let mut source = snap();
        source.favorites.tracks = vec![fav("newest", None, 0), fav("older", None, 1)];
        let p = plan(&source, &snap(), &Ledger::default());
        let ids: Vec<&str> = p.favorites.to_add[1]
            .1
            .iter()
            .map(|f| f.id.as_str())
            .collect();
        assert_eq!(ids, vec!["older", "newest"]);
    }

    #[test]
    fn ledger_entries_count_as_already_there() {
        let mut source = snap();
        source.favorites.artists = vec![fav("7", None, 0)];
        let mut ledger = Ledger::default();
        ledger.mark_favorite("artists", "7");
        let p = plan(&source, &snap(), &ledger);
        assert_eq!(p.favorites_to_add(), 0);
        assert!(p.is_empty());
    }

    #[test]
    fn playlist_rules_create_merge_and_copy() {
        let mut source = snap();
        source.playlists = vec![
            owned(10, "Road Trip", &[1, 2, 3]),
            owned(11, "Sleep", &[4, 5]),
            owned(12, "Metal", &[6, 7]),
            owned(13, "Done", &[8]),
        ];
        let mut target = snap();
        target.playlists = vec![
            // same name, subset of the source: the same playlist, behind
            owned(100, "road trip ", &[1, 2]),
            // same name, holds a foreign track: a different playlist
            owned(101, "Metal", &[6, 99]),
            // same name, identical: nothing to do
            owned(102, "Done", &[8]),
        ];
        let p = plan(&source, &target, &Ledger::default());
        assert_eq!(p.playlists_already, 1);
        assert_eq!(p.playlists.len(), 3);
        assert_eq!(
            p.playlists[0],
            PlaylistAction::Merge {
                source_id: 10,
                target_id: 100,
                name: "Road Trip".into(),
                missing_track_ids: vec![3],
            }
        );
        assert!(matches!(
            &p.playlists[1],
            PlaylistAction::Create { source_id: 11, .. }
        ));
        match &p.playlists[2] {
            PlaylistAction::CreateCopy {
                name, track_ids, ..
            } => {
                assert_eq!(name, "Metal (migrated)");
                assert_eq!(track_ids, &vec![6, 7]);
            }
            other => panic!("expected a copy, got {other:?}"),
        }
    }

    #[test]
    fn a_mapped_playlist_keeps_syncing_its_target_even_under_another_name() {
        let mut source = snap();
        source.playlists = vec![owned(10, "Renamed since", &[1, 2, 3])];
        let mut target = snap();
        target.playlists = vec![owned(500, "Old name", &[1])];
        let mut ledger = Ledger::default();
        ledger.playlist_map.insert(10, 500);
        let p = plan(&source, &target, &ledger);
        assert_eq!(
            p.playlists,
            vec![PlaylistAction::Merge {
                source_id: 10,
                target_id: 500,
                name: "Renamed since".into(),
                missing_track_ids: vec![2, 3],
            }]
        );
        // Mapped target deleted on Qobuz: treated as new again.
        target.playlists.clear();
        let p = plan(&source, &target, &ledger);
        assert!(matches!(&p.playlists[0], PlaylistAction::Create { .. }));
    }

    /// apply.rs creates the copy BEFORE the ledger records it; a crash in
    /// between left an orphan "(migrated)" playlist that the next run copied
    /// again. The orphan is found by name under the same twin rule and
    /// merged into instead.
    #[test]
    fn a_rerun_after_an_interrupted_copy_merges_into_the_orphan_copy() {
        let mut source = snap();
        source.playlists = vec![owned(12, "Metal", &[6, 7])];
        let mut target = snap();
        target.playlists = vec![
            // the foreign same-name playlist that forced the copy
            owned(101, "Metal", &[6, 99]),
            // the copy the crashed run created, never written to the ledger
            owned(300, "Metal (migrated)", &[6]),
        ];
        let p = plan(&source, &target, &Ledger::default());
        assert_eq!(
            p.playlists,
            vec![PlaylistAction::Merge {
                source_id: 12,
                target_id: 300,
                name: "Metal".into(),
                missing_track_ids: vec![7],
            }]
        );
        // A copy that holds foreign tracks is somebody else's playlist: copy again.
        target.playlists[1] = owned(300, "Metal (migrated)", &[6, 42]);
        let p = plan(&source, &target, &Ledger::default());
        assert!(matches!(&p.playlists[0], PlaylistAction::CreateCopy { .. }));
    }

    #[test]
    fn subscriptions_skip_what_the_target_already_follows_or_owns() {
        let mut source = snap();
        source.subscriptions = vec![
            SubscribedPlaylist {
                id: 1,
                name: "a".into(),
                owner_id: 9,
            },
            SubscribedPlaylist {
                id: 2,
                name: "b".into(),
                owner_id: 9,
            },
            SubscribedPlaylist {
                id: 3,
                name: "c".into(),
                owner_id: 9,
            },
        ];
        let mut target = snap();
        target.subscriptions = vec![SubscribedPlaylist {
            id: 1,
            name: "a".into(),
            owner_id: 9,
        }];
        target.playlists = vec![owned(2, "b", &[])];
        let p = plan(&source, &target, &Ledger::default());
        assert_eq!(p.subscriptions, vec![3]);
        assert_eq!(p.subscriptions_already, 2);
    }
}
