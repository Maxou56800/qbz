//! Search the sources registered by one library host.
//!
//! "Local Library" is a scope, not a file-only provider. Hosts supply their
//! enabled source adapters; adding a provider does not change this merger.
//! This is a blocking service (cached/indexed data only), independent of Qt,
//! Qobuz authentication and the network API. LocalTrack remains an internal
//! playback record, not an Orbit wire DTO: it can contain host filesystem paths.

use crate::{LibraryError, LibraryStore, LocalTrack};

type Query = dyn Fn(&str, u64) -> Result<Vec<LocalTrack>, LibraryError> + Send + Sync;

struct Provider {
    key: String,
    query: Box<Query>,
}

#[derive(Default)]
pub struct LibrarySearch {
    providers: Vec<Provider>,
}

pub struct SearchFailure {
    pub source: String,
    pub error: LibraryError,
}

#[derive(Default)]
pub struct LibrarySearchResult {
    pub tracks: Vec<LocalTrack>,
    /// One failed source must not discard successful sources' results.
    pub failures: Vec<SearchFailure>,
    /// Candidate window only: this is not an exact count of album/artist hits.
    pub has_more: bool,
}

impl LibrarySearch {
    /// The key identifies a configured source instance within this host.
    /// Native track identity and metadata are left untouched by the merger.
    pub fn add_source(
        &mut self,
        key: impl Into<String>,
        query: impl Fn(&str, u64) -> Result<Vec<LocalTrack>, LibraryError> + Send + Sync + 'static,
    ) -> Result<(), LibraryError> {
        let key = key.into();
        if key.trim().is_empty() || self.providers.iter().any(|p| p.key == key) {
            return Err(LibraryError::Other(
                "duplicate or empty search source".into(),
            ));
        }
        self.providers.push(Provider {
            key,
            query: Box::new(query),
        });
        Ok(())
    }

    /// Capture the selected profile before scheduling work, never while it runs.
    pub fn add_files(
        &mut self,
        store: LibraryStore,
        exclude_network: bool,
    ) -> Result<(), LibraryError> {
        self.add_source("files", move |q, limit| {
            store
                .read(|db| {
                    db.search_with_filter_page(q, 0, limit, true, exclude_network, "default")
                })
                .map(Option::unwrap_or_default)
        })
    }

    /// Query a bounded window per provider and interleave it fairly. A large
    /// media-server mirror cannot push all file hits out of the visible preview.
    /// The limit is global as well as per-provider, and capped at 500 candidates.
    pub fn search(&self, query: &str, limit: u64) -> LibrarySearchResult {
        let query = query.trim();
        let limit = limit.min(500) as usize;
        let mut result = LibrarySearchResult::default();
        if query.chars().count() < 2 || limit == 0 {
            return result;
        }
        let mut batches = Vec::new();
        for provider in &self.providers {
            match (provider.query)(query, limit as u64) {
                Ok(mut tracks) => {
                    result.has_more |= tracks.len() >= limit;
                    // Enforce the bound even if a provider returns too many rows.
                    tracks.truncate(limit);
                    batches.push(tracks.into_iter());
                }
                Err(error) => result.failures.push(SearchFailure {
                    source: provider.key.clone(),
                    error,
                }),
            }
        }
        while result.tracks.len() < limit {
            let before = result.tracks.len();
            for batch in &mut batches {
                if result.tracks.len() == limit {
                    break;
                }
                if let Some(track) = batch.next() {
                    result.tracks.push(track);
                }
            }
            if result.tracks.len() == before {
                break;
            }
        }
        result.has_more |= batches.iter().any(|b| b.len() > 0);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_registered_source_participates_without_identity_deduplication() {
        let mut search = LibrarySearch::default();
        // A fifth source requires registration only, not another merger branch.
        for source in ["files", "plex", "jellyfin", "subsonic", "future"] {
            search
                .add_source(source, move |q, limit| {
                    assert_eq!(q, "曲名");
                    assert_eq!(limit, 6);
                    Ok((0..20)
                        .map(|i| LocalTrack {
                            id: i,
                            source: Some(source.into()),
                            title: "Same recording".into(),
                            ..Default::default()
                        })
                        .collect())
                })
                .unwrap();
        }
        let result = search.search("  曲名  ", 6);
        assert_eq!(result.tracks.len(), 6);
        assert!(result.has_more);
        assert_eq!(
            result
                .tracks
                .iter()
                .map(|t| t.source.as_deref().unwrap())
                .collect::<Vec<_>>(),
            ["files", "plex", "jellyfin", "subsonic", "future", "files"]
        );
        assert_eq!(
            result.tracks[..5].iter().map(|t| t.id).collect::<Vec<_>>(),
            vec![0; 5]
        );
    }

    #[test]
    fn failed_provider_does_not_hide_another_library_source() {
        let mut search = LibrarySearch::default();
        search
            .add_source("unavailable", |_, _| {
                Err(LibraryError::Other("offline".into()))
            })
            .unwrap();
        search
            .add_source("working", |_, _| Ok(vec![LocalTrack::default()]))
            .unwrap();
        let result = search.search("song", 10);
        assert_eq!(result.tracks.len(), 1);
        assert_eq!(result.failures.len(), 1);
        assert_eq!(result.failures[0].source, "unavailable");
        assert!(!result.has_more);
    }

    #[test]
    fn profiles_are_captured_and_empty_queries_never_open_sources() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a/library.db");
        let b = tmp.path().join("b/library.db");
        let mut host_a = LibrarySearch::default();
        let mut host_b = LibrarySearch::default();
        host_a
            .add_files(LibraryStore::new(a.clone()), false)
            .unwrap();
        host_b
            .add_files(LibraryStore::new(b.clone()), false)
            .unwrap();
        assert!(host_a.search("song", 10).tracks.is_empty());
        assert!(host_b.search("song", 10).tracks.is_empty());
        assert!(!a.exists() && !b.exists());
        host_a
            .add_source("unused", |_, _| {
                panic!("empty queries must not query providers")
            })
            .unwrap();
        assert!(host_a.search(" 曲 ", 10).tracks.is_empty());
        assert!(host_a.search("song", 0).tracks.is_empty());
        assert!(host_a.add_source("files", |_, _| Ok(vec![])).is_err());
    }

    #[test]
    fn files_search_uses_the_selected_hosts_database() {
        let tmp = tempfile::tempdir().unwrap();
        let mut hosts = Vec::new();
        for name in ["desktop", "daemon"] {
            let store = LibraryStore::new(tmp.path().join(name).join("library.db"));
            store
                .write(|db| {
                    db.insert_scanned_track(
                        &LocalTrack {
                            file_path: "/music/song.flac".into(),
                            title: format!("Song on {name}"),
                            artist: "Artist".into(),
                            album: "Album".into(),
                            ..Default::default()
                        },
                        false,
                    )
                })
                .unwrap();
            let mut search = LibrarySearch::default();
            search.add_files(store, false).unwrap();
            hosts.push(search);
        }
        let a = hosts[0].search("Song", 20);
        let b = hosts[1].search("Song", 20);
        assert!(a.failures.is_empty() && b.failures.is_empty());
        assert_eq!(a.tracks.len(), 1);
        assert_eq!(b.tracks.len(), 1);
        assert_eq!(a.tracks[0].id, b.tracks[0].id);
        assert_eq!(a.tracks[0].title, "Song on desktop");
        assert_eq!(b.tracks[0].title, "Song on daemon");
    }
}
