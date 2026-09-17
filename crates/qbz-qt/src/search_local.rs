//! Local-library half of the search cortinilla — the on-device sections.
//!
//! Port of the local arms of `qbz/src/search.rs` (`LocalCaps`,
//! `local_artwork_url`, `local_album_artist`, `derive_local_album_rows`,
//! `derive_local_artist_rows`, `map_local_track_to_cort_row`,
//! `append_local_sections`, `load_cortinilla_local`), per the 2026-08-03
//! cortinilla-parity contract §1.3 / §2.
//!
//! It lives in its own module for two reasons. `search_qt.rs` already hosts
//! three controllers and is well past the TRACK-RULES §2 size budget, and both
//! the desktop and the immersive dropdowns need these mappers — the immersive
//! arm shipped first with its own copies, and those copies MOVED here rather
//! than being duplicated (§5: reuse over duplicate).
//!
//! Pure + blocking helpers only: no Qt types, no bridge, no `ui()`. The
//! publishing side stays in `search_qt.rs`.
//!
//! ## Search presentation and candidate policy
//!
//! - **No dedupe between the Qobuz half and the local half.** An album owned
//!   locally AND in the catalog appears twice, in two sections, with two ids.
//! - **`has_more` is window-relative, not library-relative.** No `COUNT(*)` is
//!   ever issued; it answers "were there more distinct groups inside the rows
//!   I fetched", which under-reports on a large library.
//! - Candidate reads use qbz-library's bounded, interleaved source search.
//! - **Artwork is resolved before mapping.** Current scans persist
//!   embedded/disc/collection art in that order; this bounded search window
//!   also runs the queue-time folder resolver so rows indexed by an older
//!   build get the same per-disc result without requiring a rescan.

use std::collections::HashSet;

use crate::search_qt::{CortRow, CortSection, CortinillaData};

/// The page owns a separate snapshot: typing in the dropdown must never change
/// what a page click plays. Track row keys are snapshot offsets, not DB IDs;
/// different source instances are allowed to reuse the same native integer ID.
#[derive(Default)]
pub(crate) struct PageSnapshot {
    pub data: CortinillaData,
    pub tracks: Vec<qbz_library::LocalTrack>,
}

impl PageSnapshot {
    pub fn new(query: &str, tracks: Vec<qbz_library::LocalTrack>) -> Self {
        let mut data = CortinillaData {
            query: query.into(),
            ..Default::default()
        };
        append_local_sections(
            &mut data,
            &tracks,
            LocalCaps {
                albums: 8,
                artists: 4,
                tracks: 8,
            },
            query,
        );
        for section in &mut data.sections {
            if section.kind == "local" {
                for (index, row) in section.rows.iter_mut().enumerate() {
                    row.id = index.to_string();
                }
            }
        }
        Self { data, tracks }
    }

    pub fn row(&self, index: i32) -> Option<&CortRow> {
        self.data
            .sections
            .iter()
            .flat_map(|s| &s.rows)
            .find(|r| r.flat_index == index)
    }
}

// ---------------------------------------------------------------------------
// Caps
// ---------------------------------------------------------------------------

/// Per-section caps for the LOCAL sections (`search.rs` `LocalCaps`).
///
/// The NORMAL profile (online + signed in) keeps the on-device block compact;
/// the EXPANDED profile (offline OR an unauthenticated session, where the
/// dropdown is local-dominated because Qobuz returns nothing) widens it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct LocalCaps {
    pub albums: usize,
    pub artists: usize,
    pub tracks: usize,
}

impl LocalCaps {
    /// Normal profile (Qobuz present): compact on-device block.
    pub const NORMAL: LocalCaps = LocalCaps {
        albums: 3,
        artists: 2,
        tracks: 3,
    };
    /// Expanded profile (offline / not signed in → wider on-device block).
    pub const EXPANDED: LocalCaps = LocalCaps {
        albums: 8,
        artists: 4,
        tracks: 8,
    };

    /// `expand` is offline OR an unauthenticated session.
    pub fn for_session(expand: bool) -> LocalCaps {
        if expand {
            Self::EXPANDED
        } else {
            Self::NORMAL
        }
    }

    /// How many raw local TRACK rows to fetch so the grouped album section can
    /// be filled: albums are DERIVED by grouping tracks, so one album can
    /// swallow many rows — over-fetch well beyond the shown cap. 76 normal,
    /// 136 expanded.
    pub fn fetch_limit(self) -> u64 {
        ((self.albums.max(self.tracks) * 12) + 40) as u64
    }
}

// ---------------------------------------------------------------------------
// Row builders
// ---------------------------------------------------------------------------

/// The canonical "artist" attributed to a local track for grouping: the
/// album-artist tag when present, else the track performer.
pub(crate) fn local_album_artist(t: &qbz_library::LocalTrack) -> String {
    t.album_artist
        .clone()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| t.artist.clone())
}

/// Split one source-owned artwork reference into this port's
/// `(art_url, art_path)` pair.
///
/// **This is a DELIBERATE divergence from the reference and must not be
/// "fixed" back.** The Slint stores one RAW path with any `file://` STRIPPED,
/// because its artwork dispatcher routes by scheme and decodes with
/// `fs::read`. QML needs the scheme, so here:
///
/// - a local filesystem cover becomes a `file://` url in `art_path`, handed
///   straight to QML;
/// - Plex, HTTP and source-qualified Jellyfin/Subsonic references go to
///   `art_url`, so they ride the SHARED `attach_urls` + `download_missing`
///   pipeline exactly like Qobuz rows;
/// - nothing usable yields two empty strings.
pub(crate) fn local_art_split(
    track: &qbz_library::LocalTrack,
    scope: crate::local_rows::ArtworkScope,
) -> (String, String) {
    let Some(reference) = crate::local_rows::portable_artwork_ref(track, scope) else {
        return (String::new(), String::new());
    };
    if reference.starts_with("file://") {
        (String::new(), reference)
    } else {
        (reference, String::new())
    }
}

/// Does any of `fields` contain the (already lowercased) needle?
///
/// Mirrors what the SQL does — a case-insensitive substring — so the derived
/// sections agree with the query that produced the rows. An empty needle
/// matches everything, which keeps the function honest for callers that do not
/// filter.
fn group_matches(needle: &str, fields: &[&str]) -> bool {
    if needle.is_empty() {
        return true;
    }
    fields.iter().any(|f| f.to_lowercase().contains(needle))
}

/// Exact third-line quality only when the source actually supplied enough
/// information to make the claim. `quality_detail_from_parts` deliberately
/// has playback defaults; search must not turn missing metadata into an
/// invented 16-bit / 44.1 kHz label.
fn local_quality_detail(t: &qbz_library::LocalTrack) -> String {
    if (t.bit_depth.is_none() || t.sample_rate <= 0.0)
        && !matches!(t.format, qbz_library::AudioFormat::Dsd)
    {
        return String::new();
    }
    crate::local_rows::detail_of(&t.format, t.bit_depth, t.sample_rate)
}

/// Group local TRACK rows into local ALBUM rows (`source = "local"`,
/// `kind = "album"`).
///
/// Grouped by `album_group_key` in first-seen order, **case-SENSITIVELY** —
/// unlike the artist grouping below, and that asymmetry is the reference's.
/// `id` is the group key: the click router opens the local album view with it.
/// Empty keys are skipped. `has_more` keeps counting past the cap so it stays
/// honest about the fetched window.
pub(crate) fn derive_local_album_rows(
    rows: &[qbz_library::LocalTrack],
    cap: usize,
    query: &str,
) -> (Vec<CortRow>, bool) {
    let needle = query.trim().to_lowercase();
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<CortRow> = Vec::new();
    let mut total = 0usize;
    for t in rows {
        let key = t.album_group_key.clone();
        if key.is_empty() || !seen.insert(key.clone()) {
            continue;
        }
        let title = if t.album_group_title.is_empty() {
            t.album.clone()
        } else {
            t.album_group_title.clone()
        };
        // The SQL matches at TRACK level (title OR artist OR album), so a
        // track-title hit drags its whole album in. Without this the Albums
        // section lists records whose title and artist have nothing to do with
        // the query — the section would be lying about what it is showing, and
        // the Qobuz Albums section beside it does not behave that way.
        //
        // This runs BEFORE `total` is bumped, so `has_more` counts MATCHING
        // groups only. Counting the rejected ones would light up "View more"
        // over a destination that has nothing extra to show.
        if !group_matches(&needle, &[&title, &local_album_artist(t)]) {
            continue;
        }
        total += 1;
        if out.len() >= cap {
            continue; // keep counting for an honest has_more
        }
        let (art_url, art_path) = local_art_split(t, crate::local_rows::ArtworkScope::Album);
        out.push(CortRow {
            kind: "album".into(),
            id: key,
            source: "local".into(),
            title,
            subtitle: local_album_artist(t),
            quality_detail: local_quality_detail(t),
            art_url,
            art_path,
            album_id: String::new(),
            flat_index: 0,
        });
    }
    let has_more = total > out.len();
    (out, has_more)
}

/// Group local TRACK rows into local ARTIST rows (`source = "local"`,
/// `kind = "artist"`).
///
/// Grouped by the canonical album-artist, **case-INSENSITIVELY** (the album
/// grouping above is case-sensitive — the reference's asymmetry, reproduced),
/// in first-seen order. Empty names are skipped.
///
/// **`id` is left EMPTY on purpose**: local artists have no id anywhere in the
/// system. The click router opens the LocalLibrary Artists tab by NAME, i.e.
/// the row `title`. This is why the overlay's `hasTop` must not test the id
/// for emptiness (contract B9).
pub(crate) fn derive_local_artist_rows(
    rows: &[qbz_library::LocalTrack],
    cap: usize,
    query: &str,
) -> (Vec<CortRow>, bool) {
    let needle = query.trim().to_lowercase();
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<CortRow> = Vec::new();
    let mut total = 0usize;
    for t in rows {
        let name = local_album_artist(t);
        if name.is_empty() || !seen.insert(name.to_lowercase()) {
            continue;
        }
        // Same reason as the album section: an artist whose NAME does not
        // match has no business in an Artists list, however one of their
        // tracks got matched.
        if !group_matches(&needle, &[&name]) {
            continue;
        }
        total += 1;
        if out.len() >= cap {
            continue;
        }
        let (art_url, art_path) = local_art_split(t, crate::local_rows::ArtworkScope::Album);
        out.push(CortRow {
            kind: "artist".into(),
            id: String::new(),
            source: "local".into(),
            title: name,
            subtitle: String::new(),
            quality_detail: String::new(),
            art_url,
            art_path,
            album_id: String::new(),
            flat_index: 0,
        });
    }
    let has_more = total > out.len();
    (out, has_more)
}

/// Map one `LocalTrack` to a cortinilla row tagged `source = "local"`.
///
/// `kind` stays `"track"` — it plays as a track — but the click router keys off
/// `source == "local"` to play it through the LOCAL seam rather than the Qobuz
/// media action. `id` is the library row id; the router resolves the concrete
/// `LocalTrack` back from the per-query snapshot, NOT from this id.
pub(crate) fn map_local_track_to_cort_row(t: &qbz_library::LocalTrack) -> CortRow {
    let (art_url, art_path) = local_art_split(t, crate::local_rows::ArtworkScope::Track);
    // "artist · album" when both exist, else whichever does (U+00B7).
    let subtitle = match (t.artist.is_empty(), t.album.is_empty()) {
        (false, false) => format!("{} · {}", t.artist, t.album),
        (false, true) => t.artist.clone(),
        (true, false) => t.album.clone(),
        (true, true) => String::new(),
    };
    CortRow {
        kind: "track".into(),
        id: t.id.to_string(),
        source: "local".into(),
        title: t.title.clone(),
        subtitle,
        quality_detail: local_quality_detail(t),
        art_url,
        art_path,
        // The same group key the local Albums section uses as its row id —
        // what "Open containing album" hands to `crate::open_album`.
        album_id: t.album_group_key.clone(),
        flat_index: 0,
    }
}

// ---------------------------------------------------------------------------
// Section assembly
// ---------------------------------------------------------------------------

/// Append the local "on this device" sections to a MAIN cortinilla payload,
/// placed LAST — after every Qobuz category.
///
/// Three sections in display order: **Albums**, **Artists**, **Tracks**,
/// mirroring the Qobuz section order, each capped per [`LocalCaps`]. Albums and
/// artists are DERIVED by grouping the local track rows; there is no separate
/// album or artist query.
///
/// Section `kind`s are `local-album` / `local-artist` / `local` so the "View
/// more" router opens the matching LocalLibrary tab, while the per-ROW `kind`
/// stays album/artist/track so the thumbnail shape (artists draw as circles)
/// and the row click router both behave. Empty sections are not pushed.
///
/// Re-runs `assign_flat_indices` so the local rows get contiguous flat indices
/// AFTER the Qobuz ones — this is the payload's SECOND assignment pass.
pub(crate) fn append_local_sections(
    data: &mut CortinillaData,
    rows: &[qbz_library::LocalTrack],
    caps: LocalCaps,
    query: &str,
) {
    if rows.is_empty() {
        return;
    }
    let (album_rows, albums_more) = derive_local_album_rows(rows, caps.albums, query);
    if !album_rows.is_empty() {
        data.sections.push(CortSection {
            title: qbz_i18n::t("Albums on Local Library"),
            kind: "local-album".to_string(),
            rows: album_rows,
            has_more: albums_more,
        });
    }
    let (artist_rows, artists_more) = derive_local_artist_rows(rows, caps.artists, query);
    if !artist_rows.is_empty() {
        data.sections.push(CortSection {
            title: qbz_i18n::t("Artists on Local Library"),
            kind: "local-artist".to_string(),
            rows: artist_rows,
            has_more: artists_more,
        });
    }
    let track_rows: Vec<CortRow> = rows
        .iter()
        .take(caps.tracks)
        .map(map_local_track_to_cort_row)
        .collect();
    if !track_rows.is_empty() {
        let shown = track_rows.len();
        data.sections.push(CortSection {
            title: qbz_i18n::t("On Local Library"),
            kind: "local".to_string(),
            rows: track_rows,
            has_more: rows.len() > shown,
        });
    }
    crate::search_qt::assign_flat_indices(data);
}

/// Append the local ALBUM section to an IMMERSIVE payload — albums ONLY,
/// because selecting a row there queues it rather than navigating. No "View
/// more" in immersive, so `has_more` is carried and unused.
pub(crate) fn append_immersive_local_albums(
    data: &mut CortinillaData,
    rows: &[qbz_library::LocalTrack],
    cap: usize,
    query: &str,
) {
    if rows.is_empty() {
        return;
    }
    let (album_rows, has_more) = derive_local_album_rows(rows, cap, query);
    if album_rows.is_empty() {
        return;
    }
    data.sections.push(CortSection {
        title: qbz_i18n::t("Albums on Local Library"),
        kind: "local-album".to_string(),
        rows: album_rows,
        has_more,
    });
    crate::search_qt::assign_flat_indices(data);
}

// ---------------------------------------------------------------------------
// The fetch
// ---------------------------------------------------------------------------

/// Fetch up to `limit` local-library tracks matching `query`, off the calling
/// thread.
///
/// Callers run this alongside `core.search_all` with `tokio::join!`.
/// Local lookup has no Qobuz-auth prerequisite; publication still waits for
/// both queries, and a Qobuz failure must preserve the local result.
///
/// `gated` is the intelligent-search kill switch. The MAIN cortinilla passes
/// `true` — the module being off means no local search either, which is the
/// reference's behaviour. The IMMERSIVE search passes `false`: it is governed
/// by its own "search action" enable.
pub(crate) async fn load_cortinilla_local(
    query: String,
    limit: u64,
    gated: bool,
) -> Vec<qbz_library::LocalTrack> {
    if gated && !crate::search_qt::is_enabled() {
        log::info!("[qbz-qt] cortinilla local: gated off (intelligent-search disabled)");
        return Vec::new();
    }
    let q = query.trim().to_string();
    if q.chars().count() < 2 {
        return Vec::new();
    }
    let exclude_network = crate::offline_fwd::exclude_network_folders_now();
    // Resolve the files profile on the caller before scheduling work. Remote
    // adapters retain their existing cache/config ownership until the per-host
    // source registry is extracted; the shared merger has no Qt globals.
    let mut search = qbz_library::search::LibrarySearch::default();
    if let Some(path) = crate::local_state::db_path() {
        search
            .add_files(qbz_library::LibraryStore::new(path), exclude_network)
            .expect("files source registered once");
    }
    if crate::local_plex::is_configured() {
        search
            .add_source("plex", |q, limit| {
                Ok(crate::local_plex::search_tracks_page(
                    q,
                    0,
                    limit,
                    "default",
                    &[],
                    false,
                    &[],
                ))
            })
            .expect("Plex source registered once");
    }
    for source in crate::media_servers_qt::configured_words() {
        search
            .add_source(source, move |q, limit| {
                Ok(crate::media_servers_qt::search_tracks_page(
                    source,
                    q,
                    0,
                    limit,
                    "default",
                    &[],
                    false,
                    &[],
                ))
            })
            .expect("media source registered once");
    }
    let t = std::time::Instant::now();
    let rows = tokio::task::spawn_blocking(move || {
        let result = search.search(&q, limit);
        for failure in result.failures {
            log::warn!(
                "[qbz-qt] local search source {}: {}",
                failure.source,
                failure.error
            );
        }
        let mut rows = result.tracks;
        crate::local_playback::fill_missing_covers(&mut rows);
        rows
    })
    .await
    .unwrap_or_default();
    log::info!(
        "[qbz-qt][perf] local search limit={limit} -> {} rows in {:?}",
        rows.len(),
        t.elapsed()
    );
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(title: &str, artist: &str, album: &str) -> qbz_library::LocalTrack {
        let mut t = qbz_library::LocalTrack::default();
        t.title = title.into();
        t.artist = artist.into();
        t.album = album.into();
        t.album_group_key = format!("{artist}|{album}");
        t.album_group_title = album.into();
        t
    }

    #[test]
    fn page_snapshot_preserves_colliding_source_ids_and_is_independent_of_dropdown() {
        let mut files = track("Song", "Artist", "Album");
        files.id = 7;
        let mut remote = files.clone();
        remote.source = Some("jellyfin".into());
        remote.file_path = "server-item-7".into();
        let page = PageSnapshot::new("Song", vec![files, remote]);
        let dropdown = PageSnapshot::new("Other", vec![track("Other", "Other", "Other")]);
        let section = page
            .data
            .sections
            .iter()
            .find(|s| s.kind == "local")
            .unwrap();
        assert_eq!(section.rows.len(), 2);
        for (offset, expected_source) in [None, Some("jellyfin")].iter().enumerate() {
            let displayed = page.row(section.rows[offset].flat_index).unwrap();
            let resolved = &page.tracks[displayed.id.parse::<usize>().unwrap()];
            assert_eq!(resolved.id, 7);
            assert_eq!(&resolved.source.as_deref(), expected_source);
            assert_eq!(resolved.title, "Song");
        }
        assert_eq!(dropdown.tracks[0].title, "Other");
        assert!(page.row(-1).is_none());
        assert!(page.row(1000).is_none());
    }

    /// The owner's real report: searching "Iro" showed Cynic and Die Toten
    /// Hosen under "Artists on Local Library", and their records under
    /// "Albums on Local Library". Neither name contains "iro" - a TRACK of
    /// theirs matched, and the derivation dragged the whole group in.
    #[test]
    fn derived_sections_only_keep_groups_that_match_the_query() {
        let rows = vec![
            // Matches on the TRACK title only. Must NOT produce an album or
            // artist row: neither "Cynic" nor "Uroboric Forms" contains "iro".
            track("Iroquois Dawn", "Cynic", "Uroboric Forms"),
            // Matches on the ARTIST name -> belongs in both derived sections.
            track("Run to the Hills", "Iron Maiden", "The Number of the Beast"),
            // Matches on the ALBUM title -> album row yes, artist row no.
            track("Some Song", "El Consorcio", "Iron Canciones"),
        ];

        let (albums, _) = derive_local_album_rows(&rows, 10, "Iro");
        let album_titles: Vec<&str> = albums.iter().map(|r| r.title.as_str()).collect();
        assert!(
            !album_titles.contains(&"Uroboric Forms"),
            "a track-title match must not pull its album in: {album_titles:?}"
        );
        assert!(
            album_titles.contains(&"The Number of the Beast"),
            "album-artist match"
        );
        assert!(
            album_titles.contains(&"Iron Canciones"),
            "album-title match"
        );

        let (artists, _) = derive_local_artist_rows(&rows, 10, "Iro");
        let names: Vec<&str> = artists.iter().map(|r| r.title.as_str()).collect();
        assert_eq!(names, vec!["Iron Maiden"], "only NAME matches belong here");
    }

    /// The TRACKS section is unaffected - a track that matched on its title is
    /// exactly what that section is for.
    #[test]
    fn track_rows_are_not_filtered_by_the_group_rule() {
        let mut t = track("Iroquois Dawn", "Cynic", "Uroboric Forms");
        t.format = qbz_library::AudioFormat::Flac;
        t.bit_depth = Some(24);
        t.sample_rate = 96_000.0;
        let row = map_local_track_to_cort_row(&t);
        assert_eq!(row.title, "Iroquois Dawn");
        assert_eq!(row.kind, "track");
        assert_eq!(row.source, "local");
        assert_eq!(row.quality_detail, "24-bit / 96 kHz");
    }

    #[test]
    fn remote_collection_art_keeps_its_source_in_search_rows() {
        let mut jellyfin = track("The Writing on the Wall", "Iron Maiden", "Senjutsu");
        jellyfin.source = Some("jellyfin".into());
        jellyfin.artwork_path = None;
        jellyfin.collection_artwork_path = Some("jf-album/tag".into());
        let (albums, _) = derive_local_album_rows(&[jellyfin], 3, "Senjutsu");
        assert_eq!(albums[0].art_url, "jellyfin:jf-album/tag");

        let mut navidrome = track("Song", "Artist", "Navidrome Album");
        navidrome.source = Some("navidrome".into());
        navidrome.artwork_path = None;
        navidrome.collection_artwork_path = Some("al-42_hash".into());
        let row = map_local_track_to_cort_row(&navidrome);
        assert_eq!(row.art_url, "subsonic:al-42_hash");
    }

    /// has_more must describe the MATCHING set. Counting rejected groups
    /// would light up "View more" over a destination with nothing extra.
    #[test]
    fn has_more_counts_only_matching_groups() {
        let rows = vec![
            track("Iron Song", "Iron Maiden", "Iron Album"),
            // Six non-matching groups: without the fix these inflate `total`
            // and has_more comes back true over a single matching album.
            track("Iroquois", "A", "AA"),
            track("Iroquois", "B", "BB"),
            track("Iroquois", "C", "CC"),
            track("Iroquois", "D", "DD"),
            track("Iroquois", "E", "EE"),
            track("Iroquois", "F", "FF"),
        ];
        let (albums, has_more) = derive_local_album_rows(&rows, 3, "Iro");
        assert_eq!(albums.len(), 1, "only one album group matches");
        assert!(!has_more, "one match under a cap of 3 leaves nothing more");

        let (artists, artists_more) = derive_local_artist_rows(&rows, 3, "Iro");
        assert_eq!(artists.len(), 1);
        assert!(!artists_more);
    }

    #[test]
    fn an_empty_query_filters_nothing() {
        let rows = vec![track("A", "B", "C")];
        let (albums, _) = derive_local_album_rows(&rows, 10, "");
        assert_eq!(
            albums.len(),
            1,
            "an empty needle must not empty the section"
        );
    }
}
