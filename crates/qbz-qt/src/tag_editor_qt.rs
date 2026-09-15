//! Qt local-album metadata editor controller.
//!
//! The selected physical version is snapshotted on open. QML edits a bounded
//! DTO and returns row ids only; paths are always resolved against this Rust
//! snapshot before sidecar or embedded-tag writes.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use cxx_qt_lib::QString;
use qbz_library::{
    AlbumExtendedMetadataOverride, AlbumMetadataOverride, AlbumTagInspection, AlbumTagSidecar,
    AlbumTagWrite, DirectTagWriteOptions, ExtendedAlbumTagWrite, ExtendedTrackTagWrite,
    FrontCoverWrite, Id3v2WriteVersion, LocalTrack, TrackExtendedMetadataOverride,
    TrackMetadataOverride, TrackMetadataUpdateFull, TrackTagWrite,
};
use serde::{Deserialize, Serialize};

#[derive(Clone)]
enum EditorTarget {
    Library,
    Ephemeral {
        session_path: String,
    },
    Remote {
        target: qbz_library::RemoteTagTarget,
    },
}

#[derive(Clone)]
struct EditorSession {
    generation: u64,
    album_id: String,
    group_key: String,
    directory: String,
    tracks: Vec<LocalTrack>,
    target: EditorTarget,
    staged_artwork: Option<StagedArtwork>,
    artwork_candidates: HashMap<String, ArtworkCandidate>,
}

#[derive(Clone)]
struct StagedArtwork {
    token: String,
    bytes: Vec<u8>,
    preview_path: String,
    source: String,
    width: u32,
    height: u32,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ArtworkCandidate {
    id: String,
    preview_url: String,
    source: String,
    title: String,
    detail: String,
}

static SESSION: OnceLock<Mutex<Option<EditorSession>>> = OnceLock::new();
static OPEN_GEN: AtomicU64 = AtomicU64::new(0);
static SAVE_GEN: AtomicU64 = AtomicU64::new(0);
static REMOTE_GEN: AtomicU64 = AtomicU64::new(0);
static ARTWORK_GEN: AtomicU64 = AtomicU64::new(0);
static REMOTE_SEQ: AtomicI32 = AtomicI32::new(0);
static ARTWORK_SEQ: AtomicU64 = AtomicU64::new(0);
static SAVE_ACTIVE: AtomicBool = AtomicBool::new(false);
static TRACK_MODAL_ACTIVE: AtomicBool = AtomicBool::new(false);

fn session() -> &'static Mutex<Option<EditorSession>> {
    SESSION.get_or_init(|| Mutex::new(None))
}

/// Recover the physical source from the authoritative row. Older/cache rows
/// can carry an empty or folded `source`, while their album identity remains
/// explicitly namespaced. Treating those rows as local turns the server item
/// id in `file_path` into a filesystem path and produces the misleading
/// "local file versions only" toast.
fn editor_remote_source(track: &LocalTrack) -> String {
    let direct =
        crate::remote_metadata_qt::canonical_source(track.source.as_deref().unwrap_or_default());
    if !direct.is_empty() {
        return direct.to_string();
    }
    track
        .album_group_key
        .split_once(':')
        .map(|(prefix, _)| crate::remote_metadata_qt::canonical_source(prefix))
        .filter(|source| !source.is_empty())
        .unwrap_or_default()
        .to_string()
}

/// The provider's album id inside a media-server album group key: Plex groups
/// as `plex:album:<rating key>`, Jellyfin and Subsonic as `<source>:<id>`.
fn remote_album_id(source: &str, group_key: &str) -> String {
    let prefix = if source == "plex" {
        "plex:album:".to_string()
    } else {
        format!("{source}:")
    };
    group_key
        .strip_prefix(&prefix)
        .unwrap_or(group_key)
        .to_string()
}

/// The effective album group of a media-server album's rows (the first row's
/// own key wins over the caller's fallback) and the sidecar target it edits.
/// Media-server files live on the server, so the only writable layer is QBZ's
/// own metadata sidecar — `validate_draft` refuses a direct write for it.
fn remote_editor_target(source: &str, tracks: &[LocalTrack], fallback_group: &str) -> (String, EditorTarget) {
    let group = tracks
        .first()
        .map(|track| track.album_group_key.as_str())
        .filter(|key| !key.is_empty())
        .unwrap_or(fallback_group)
        .to_string();
    let target = EditorTarget::Remote {
        target: crate::remote_metadata_qt::target(
            source,
            &crate::remote_metadata_qt::active_source_instance(source),
            &remote_album_id(source, &group),
        ),
    };
    (group, target)
}

/// Sidecars belong to an album directory, not to the album identity string.
/// Folder grouping normally makes both values equal, but metadata grouping
/// deliberately does not. Prefer a real group directory, then the selected
/// file's containing folder. Requiring the selected path itself to be a file
/// was unnecessarily strict (a renamed/missing row can still be corrected in
/// a valid album sidecar) and broke mounted network libraries in particular.
fn editor_directory(track: &LocalTrack) -> Option<String> {
    if let Some(reference) = qbz_disc::SacdRef::parse(&track.file_path) {
        return reference
            .image
            .parent()
            .filter(|path| path.is_dir())
            .map(|path| path.to_string_lossy().into_owned());
    }
    let group = Path::new(&track.album_group_key);
    if group.is_dir() {
        return Some(group.to_string_lossy().into_owned());
    }
    Path::new(&track.file_path)
        .parent()
        .filter(|directory| directory.is_dir())
        .map(|directory| directory.to_string_lossy().into_owned())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TagLayerDoc {
    name: String,
    file_count: usize,
    canonical_file_count: usize,
    writable_file_count: usize,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct InspectionDoc {
    file_count: usize,
    canonical_layers: Vec<TagLayerDoc>,
    present_layers: Vec<TagLayerDoc>,
    conflicting_files: usize,
    writable_files: usize,
    direct_write_supported: bool,
    error: String,
}

impl InspectionDoc {
    fn from_result(result: Result<AlbumTagInspection, qbz_library::LibraryError>) -> Self {
        match result {
            Ok(inspection) => {
                let direct_write_supported = inspection.direct_write_supported();
                Self {
                    file_count: inspection.file_count,
                    canonical_layers: inspection
                        .canonical_layers
                        .into_iter()
                        .map(|layer| TagLayerDoc {
                            name: layer.name,
                            file_count: layer.file_count,
                            canonical_file_count: layer.canonical_file_count,
                            writable_file_count: layer.writable_file_count,
                        })
                        .collect(),
                    present_layers: inspection
                        .present_layers
                        .into_iter()
                        .map(|layer| TagLayerDoc {
                            name: layer.name,
                            file_count: layer.file_count,
                            canonical_file_count: layer.canonical_file_count,
                            writable_file_count: layer.writable_file_count,
                        })
                        .collect(),
                    conflicting_files: inspection.conflicting_files,
                    writable_files: inspection.writable_files,
                    direct_write_supported,
                    error: String::new(),
                }
            }
            Err(error) => Self {
                file_count: 0,
                canonical_layers: Vec::new(),
                present_layers: Vec::new(),
                conflicting_files: 0,
                writable_files: 0,
                direct_write_supported: false,
                error: error.to_string(),
            },
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct TrackSeed {
    id: String,
    file_name: String,
    title: String,
    track_number: String,
    disc_number: String,
    cue_based: bool,
    artist_credit: String,
    artists: Vec<String>,
    composers: Vec<String>,
    performers: Vec<String>,
    musicbrainz_recording_id: String,
    musicbrainz_track_id: String,
    musicbrainz_artist_ids: Vec<String>,
}

#[derive(Serialize, Default)]
#[serde(rename_all = "camelCase")]
struct ArtworkSeed {
    preview_path: String,
    token: String,
    source: String,
    width: u32,
    height: u32,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct EditorSeed {
    album_id: String,
    group_key: String,
    directory: String,
    album_title: String,
    album_artist: String,
    album_artists: Vec<String>,
    compilation: bool,
    year: String,
    genre: String,
    catalog_number: String,
    total_discs: u32,
    musicbrainz_release_id: String,
    musicbrainz_release_group_id: String,
    musicbrainz_album_artist_ids: Vec<String>,
    discogs_release_id: String,
    artwork: ArtworkSeed,
    sidecar_exists: bool,
    remote_sidecar_only: bool,
    fixed_track_numbers: bool,
    can_direct_write: bool,
    direct_write_reason: String,
    inspection: InspectionDoc,
    tracks: Vec<TrackSeed>,
}

fn build_seed(open: &EditorSession) -> EditorSeed {
    let tracks = &open.tracks;
    let sacd_image = tracks
        .first()
        .and_then(|track| qbz_disc::SacdRef::parse(&track.file_path))
        .map(|reference| reference.image);
    let remote_target = match &open.target {
        EditorTarget::Remote { target } => Some(target),
        _ => None,
    };
    let cue_based = tracks
        .iter()
        .any(|track| track.cue_file_path.is_some() || track.cue_start_secs.is_some());
    let paths = if remote_target.is_some() || sacd_image.is_some() {
        Vec::new()
    } else {
        tracks
            .iter()
            .map(|track| track.file_path.clone())
            .collect::<Vec<_>>()
    };
    let inspection = if remote_target.is_some() || sacd_image.is_some() {
        InspectionDoc {
            file_count: 0,
            canonical_layers: Vec::new(),
            present_layers: Vec::new(),
            conflicting_files: 0,
            writable_files: 0,
            direct_write_supported: false,
            error: String::new(),
        }
    } else {
        InspectionDoc::from_result(qbz_library::inspect_album_tag_layers(&paths))
    };
    let snapshots = if remote_target.is_some() || sacd_image.is_some() {
        Vec::new()
    } else {
        qbz_library::read_editor_tag_snapshots(&paths)
    };
    let snapshot_by_path = snapshots
        .iter()
        .map(|snapshot| (snapshot.file_path.as_str(), snapshot))
        .collect::<HashMap<_, _>>();
    let sidecar = remote_target
        .and_then(crate::remote_metadata_qt::sidecar)
        .or_else(|| {
            (remote_target.is_none())
                .then(|| match sacd_image.as_ref() {
                    Some(image) => qbz_library::read_sacd_sidecar(image),
                    None => qbz_library::read_album_sidecar(Path::new(&open.directory)),
                })
                .and_then(Result::ok)
                .flatten()
        });
    let extended_sidecar = sidecar
        .as_ref()
        .and_then(|sidecar| sidecar.extended_album.as_ref());
    let local_files = remote_target.is_none()
        && tracks
            .iter()
            .all(|track| Path::new(&track.file_path).is_file());
    let can_direct_write = remote_target.is_none()
        && sacd_image.is_none()
        && !cue_based
        && local_files
        && inspection.direct_write_supported;
    let direct_write_reason = if remote_target.is_some() {
        qbz_i18n::t("Media-server metadata is stored in a local sidecar.")
    } else if sacd_image.is_some() {
        qbz_i18n::t("SACD images use sidecar metadata.")
    } else if cue_based {
        qbz_i18n::t("CUE-based albums use sidecar metadata.")
    } else if !local_files {
        qbz_i18n::t("One or more audio files are unavailable.")
    } else if !inspection.error.is_empty() {
        inspection.error.clone()
    } else if !inspection.direct_write_supported {
        qbz_i18n::t("The canonical tag is not writable for every file.")
    } else {
        String::new()
    };

    let first = tracks.first();
    let album_title = first
        .map(|track| {
            if track.album_group_title.trim().is_empty() {
                track.album.trim().to_string()
            } else {
                track.album_group_title.trim().to_string()
            }
        })
        .unwrap_or_default();
    let album_artist = qbz_library::compute_track_artist_match(tracks).unwrap_or_default();
    let first_snapshot = snapshots.first();
    let mut album_artists = first_snapshot
        .map(|snapshot| snapshot.album_artists.clone())
        .unwrap_or_default();
    if let Some(sidecar) = extended_sidecar {
        if !sidecar.album_artists.is_empty() {
            album_artists = sidecar.album_artists.clone();
        }
    }
    if album_artists.is_empty() && !album_artist.trim().is_empty() {
        album_artists.push(album_artist.clone());
    }
    let year = tracks
        .iter()
        .find_map(|track| track.year)
        .map(|year| year.to_string())
        .unwrap_or_default();
    let genre = tracks
        .iter()
        .find_map(|track| {
            track
                .genre
                .as_ref()
                .filter(|value| !value.trim().is_empty())
        })
        .map(|value| value.trim().to_string())
        .unwrap_or_default();
    let catalog_number = tracks
        .iter()
        .find_map(|track| {
            track
                .catalog_number
                .as_ref()
                .filter(|value| !value.trim().is_empty())
        })
        .map(|value| value.trim().to_string())
        .unwrap_or_default();
    let total_discs = tracks
        .iter()
        .filter_map(|track| track.disc_number)
        .max()
        .unwrap_or(1)
        .max(1);
    let artwork_path = open
        .staged_artwork
        .as_ref()
        .map(|artwork| artwork.preview_path.clone())
        .or_else(|| {
            sidecar
                .as_ref()
                .and_then(|sidecar| sidecar.extended_album.as_ref())
                .and_then(|album| album.artwork_path.clone())
                .filter(|path| Path::new(path).is_file())
        })
        .or_else(|| {
            tracks
                .iter()
                .filter_map(|track| track.artwork_path.as_ref())
                .find(|path| Path::new(path).is_file())
                .cloned()
        })
        .unwrap_or_default();
    let artwork = open
        .staged_artwork
        .as_ref()
        .map(|artwork| ArtworkSeed {
            preview_path: crate::artwork_qt::file_url(&artwork.preview_path),
            token: artwork.token.clone(),
            source: artwork.source.clone(),
            width: artwork.width,
            height: artwork.height,
        })
        .unwrap_or_else(|| ArtworkSeed {
            preview_path: if artwork_path.is_empty() {
                String::new()
            } else {
                crate::artwork_qt::file_url(&artwork_path)
            },
            source: if artwork_path.is_empty() {
                String::new()
            } else {
                qbz_i18n::t("Current artwork")
            },
            ..ArtworkSeed::default()
        });
    let compilation = extended_sidecar
        .and_then(|extended| extended.compilation)
        .or_else(|| first_snapshot.and_then(|snapshot| snapshot.compilation))
        .unwrap_or(false);
    let musicbrainz_release_id = extended_sidecar
        .and_then(|extended| extended.musicbrainz_release_id.clone())
        .or_else(|| first_snapshot.and_then(|snapshot| snapshot.musicbrainz_release_id.clone()))
        .unwrap_or_default();
    let musicbrainz_release_group_id = extended_sidecar
        .and_then(|extended| extended.musicbrainz_release_group_id.clone())
        .or_else(|| {
            first_snapshot.and_then(|snapshot| snapshot.musicbrainz_release_group_id.clone())
        })
        .unwrap_or_default();
    let musicbrainz_album_artist_ids = extended_sidecar
        .map(|extended| extended.musicbrainz_album_artist_ids.clone())
        .filter(|ids| !ids.is_empty())
        .or_else(|| first_snapshot.map(|snapshot| snapshot.musicbrainz_album_artist_ids.clone()))
        .unwrap_or_default();
    let discogs_release_id = extended_sidecar
        .and_then(|extended| extended.discogs_release_id.clone())
        .unwrap_or_default();

    EditorSeed {
        album_id: open.album_id.clone(),
        group_key: open.group_key.clone(),
        directory: open.directory.clone(),
        album_title,
        album_artist,
        album_artists,
        compilation,
        year,
        genre,
        catalog_number,
        total_discs,
        musicbrainz_release_id,
        musicbrainz_release_group_id,
        musicbrainz_album_artist_ids,
        discogs_release_id,
        artwork,
        sidecar_exists: if remote_target.is_some() {
            sidecar.is_some()
        } else {
            sacd_image
                .as_ref()
                .map(|image| qbz_library::sacd_sidecar_path(image))
                .unwrap_or_else(|| qbz_library::sidecar_path(Path::new(&open.directory)))
                .exists()
        },
        remote_sidecar_only: remote_target.is_some(),
        fixed_track_numbers: sacd_image.is_some(),
        can_direct_write,
        direct_write_reason,
        inspection,
        tracks: tracks
            .iter()
            .map(|track| {
                let snapshot = snapshot_by_path.get(track.file_path.as_str()).copied();
                let extended = sidecar.as_ref().and_then(|sidecar| {
                    sidecar.extended_tracks.iter().find(|entry| {
                        entry.file_path == track.file_path
                            && match (entry.cue_start_secs, track.cue_start_secs) {
                                (Some(a), Some(b)) => (a - b).abs() < 0.001,
                                (None, None) => true,
                                _ => false,
                            }
                    })
                });
                let artist_credit = extended
                    .map(|entry| entry.artist_credit.clone())
                    .filter(|artist| !artist.trim().is_empty())
                    .or_else(|| snapshot.map(|snapshot| snapshot.artist_credit.clone()))
                    .filter(|artist| !artist.trim().is_empty())
                    .unwrap_or_else(|| track.artist.clone());
                let artists = extended
                    .map(|entry| entry.artists.clone())
                    .filter(|artists| !artists.is_empty())
                    .or_else(|| snapshot.map(|snapshot| snapshot.artists.clone()))
                    .filter(|artists| !artists.is_empty())
                    .unwrap_or_else(|| vec![artist_credit.clone()]);
                TrackSeed {
                    id: track.id.to_string(),
                    file_name: if remote_target.is_some() {
                        track.file_path.clone()
                    } else {
                        Path::new(&track.file_path)
                            .file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or(&track.file_path)
                            .to_string()
                    },
                    title: track.title.clone(),
                    track_number: track
                        .track_number
                        .map(|number| number.to_string())
                        .unwrap_or_default(),
                    disc_number: track
                        .disc_number
                        .map(|number| number.to_string())
                        .unwrap_or_default(),
                    cue_based: track.cue_file_path.is_some() || track.cue_start_secs.is_some(),
                    artist_credit,
                    artists,
                    composers: extended
                        .map(|entry| entry.composers.clone())
                        .or_else(|| snapshot.map(|snapshot| snapshot.composers.clone()))
                        .unwrap_or_default(),
                    performers: extended
                        .map(|entry| entry.performers.clone())
                        .or_else(|| snapshot.map(|snapshot| snapshot.performers.clone()))
                        .unwrap_or_default(),
                    musicbrainz_recording_id: extended
                        .and_then(|entry| entry.musicbrainz_recording_id.clone())
                        .or_else(|| {
                            snapshot.and_then(|snapshot| snapshot.musicbrainz_recording_id.clone())
                        })
                        .unwrap_or_default(),
                    musicbrainz_track_id: extended
                        .and_then(|entry| entry.musicbrainz_track_id.clone())
                        .or_else(|| {
                            snapshot.and_then(|snapshot| snapshot.musicbrainz_track_id.clone())
                        })
                        .unwrap_or_default(),
                    musicbrainz_artist_ids: extended
                        .map(|entry| entry.musicbrainz_artist_ids.clone())
                        .filter(|ids| !ids.is_empty())
                        .or_else(|| {
                            snapshot.map(|snapshot| snapshot.musicbrainz_artist_ids.clone())
                        })
                        .unwrap_or_default(),
                }
            })
            .collect(),
    }
}

fn open_session(
    album_id: String,
    group_key: String,
    directory: String,
    tracks: Vec<LocalTrack>,
    target: EditorTarget,
    track_index: Option<usize>,
) {
    if SAVE_ACTIVE.load(Ordering::Acquire) {
        return;
    }
    if tracks.is_empty() {
        crate::toast_qt::error(qbz_i18n::t("No local album version is selected."));
        return;
    }
    let remote = matches!(target, EditorTarget::Remote { .. });
    if !remote && (directory.trim().is_empty() || !Path::new(&directory).is_dir()) {
        crate::toast_qt::info(qbz_i18n::t(
            "Metadata editing is available for local file versions only.",
        ));
        return;
    }

    let generation = OPEN_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    let open = EditorSession {
        generation,
        album_id,
        group_key,
        directory,
        tracks,
        target,
        staged_artwork: None,
        artwork_candidates: HashMap::new(),
    };
    *session().lock().expect("tag editor session lock") = Some(open.clone());
    crate::tag_editor_bridge::ui(move |mut bridge| {
        let modal = track_index.is_some();
        bridge.as_mut().set_editor_open(!modal);
        bridge.as_mut().set_track_editor_open(modal);
        bridge.as_mut().set_track_editor_initial_index(
            track_index
                .and_then(|index| i32::try_from(index).ok())
                .unwrap_or(-1),
        );
        bridge.as_mut().set_editor_loading(true);
        bridge.as_mut().set_editor_saving(false);
        bridge.as_mut().set_editor_progress_current(0);
        bridge.as_mut().set_editor_progress_total(0);
        bridge.as_mut().set_editor_json(QString::from("{}"));
        bridge.as_mut().set_remote_json(QString::from("{}"));
        bridge.as_mut().set_remote_searching(false);
        bridge.as_mut().set_remote_loading(false);
        bridge.as_mut().set_artwork_searching(false);
        bridge.as_mut().set_artwork_loading(false);
    });
    TRACK_MODAL_ACTIVE.store(track_index.is_some(), Ordering::Release);
    if track_index.is_none() {
        crate::nav_qt::record("metadataeditor");
    }

    crate::spawn(async move {
        let started = std::time::Instant::now();
        let row_count = open.tracks.len();
        let seed = tokio::task::spawn_blocking(move || build_seed(&open)).await;
        if OPEN_GEN.load(Ordering::SeqCst) != generation {
            return;
        }
        match seed {
            Ok(seed) => {
                log::info!(
                    "[qbz-qt][perf] tag editor preflight: {} rows in {:?}",
                    row_count,
                    started.elapsed()
                );
                let json = serde_json::to_string(&seed).unwrap_or_else(|_| "{}".to_string());
                crate::tag_editor_bridge::ui(move |mut bridge| {
                    bridge
                        .as_mut()
                        .set_editor_json(QString::from(json.as_str()));
                    bridge.as_mut().set_editor_loading(false);
                });
            }
            Err(error) => {
                log::error!("[qbz-qt] tag editor preflight task failed: {error}");
                crate::toast_qt::error(qbz_i18n::t("Couldn't inspect album metadata."));
                close();
            }
        }
    });
}

pub fn open(album_id: String) {
    let tracks = crate::local_album_actions::current_version_tracks();
    let group_key = crate::local_album_actions::current_version_dir();
    // A Plex / Jellyfin / Subsonic version has no local directory: it edits
    // its sidecar, exactly like the per-track editor already does. Without
    // this arm the album header's pencil fell into the local-directory check
    // and answered "available for local file versions only".
    let source = tracks.first().map(editor_remote_source).unwrap_or_default();
    if !source.is_empty() {
        let (group, target) = remote_editor_target(&source, &tracks, &group_key);
        open_session(album_id, group, String::new(), tracks, target, None);
        return;
    }
    open_session(
        album_id,
        group_key.clone(),
        tracks
            .first()
            .and_then(editor_directory)
            .unwrap_or(group_key),
        tracks,
        EditorTarget::Library,
        None,
    );
}

pub fn open_ephemeral(group_key: String) {
    let Some((session_path, directory, tracks)) =
        crate::local_ephemeral::editor_snapshot(&group_key)
    else {
        crate::toast_qt::info(qbz_i18n::t(
            "Metadata editing is available for local file versions only.",
        ));
        return;
    };
    open_session(
        format!("ephemeral:{group_key}"),
        directory.clone(),
        directory,
        tracks,
        EditorTarget::Ephemeral { session_path },
        None,
    );
}

/// Open the compact per-track editor on a real local row. The album siblings
/// are resolved from the authoritative library database, never from the
/// resident/paged QML model, so previous/next remains complete regardless of
/// virtualization or the active sort.
pub fn open_track(track: LocalTrack) {
    let selected_id = track.id;
    let group_key = track.album_group_key.clone();
    let source = editor_remote_source(&track);
    let remote = !source.is_empty();
    if group_key.trim().is_empty() {
        crate::toast_qt::info(qbz_i18n::t(
            "Metadata editing is available for local file versions only.",
        ));
        return;
    }
    let local_directory = (!remote).then(|| editor_directory(&track)).flatten();
    crate::spawn(async move {
        let lookup_key = group_key.clone();
        let tracks = tokio::task::spawn_blocking(move || {
            if remote {
                crate::local_albums::fetch_album_tracks_blocking(&lookup_key)
            } else {
                crate::local_state::with_db(|db| db.get_album_tracks(&lookup_key))
                    .unwrap_or_default()
            }
        })
        .await
        .unwrap_or_default();
        if !remote && local_directory.is_none() {
            crate::toast_qt::info(qbz_i18n::t(
                "The local album folder is not currently available.",
            ));
            return;
        }
        let Some(index) = tracks.iter().position(|row| row.id == selected_id) else {
            crate::toast_qt::info(qbz_i18n::t("The track is no longer in the library."));
            return;
        };
        let target = if remote {
            remote_editor_target(&source, &tracks, &group_key).1
        } else {
            EditorTarget::Library
        };
        open_session(
            group_key.clone(),
            group_key.clone(),
            if remote {
                String::new()
            } else {
                local_directory.unwrap_or(group_key)
            },
            tracks,
            target,
            Some(index),
        );
    });
}

pub fn promote_to_album_editor() {
    if session().lock().expect("tag editor session lock").is_none()
        || SAVE_ACTIVE.load(Ordering::Acquire)
    {
        return;
    }
    TRACK_MODAL_ACTIVE.store(false, Ordering::Release);
    crate::tag_editor_bridge::ui(|mut bridge| {
        bridge.as_mut().set_track_editor_open(false);
        bridge.as_mut().set_editor_open(true);
    });
    crate::nav_qt::record("metadataeditor");
}

fn clear_session() {
    if SAVE_ACTIVE.load(Ordering::Acquire) {
        return;
    }
    OPEN_GEN.fetch_add(1, Ordering::SeqCst);
    REMOTE_GEN.fetch_add(1, Ordering::SeqCst);
    ARTWORK_GEN.fetch_add(1, Ordering::SeqCst);
    let prior = session().lock().expect("tag editor session lock").take();
    TRACK_MODAL_ACTIVE.store(false, Ordering::Release);
    if let Some(path) = prior
        .and_then(|session| session.staged_artwork)
        .map(|artwork| artwork.preview_path)
        .filter(|path| {
            Path::new(path)
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(".tag-editor-stage-"))
        })
    {
        let _ = std::fs::remove_file(path);
    }
    crate::tag_editor_bridge::ui(|mut bridge| {
        bridge.as_mut().set_editor_open(false);
        bridge.as_mut().set_track_editor_open(false);
        bridge.as_mut().set_track_editor_initial_index(-1);
        bridge.as_mut().set_editor_loading(false);
        bridge.as_mut().set_remote_searching(false);
        bridge.as_mut().set_remote_loading(false);
        bridge.as_mut().set_artwork_searching(false);
        bridge.as_mut().set_artwork_loading(false);
    });
}

pub fn close() {
    clear_session();
    if crate::nav_qt::current_view() == "metadataeditor" {
        crate::nav_qt::back();
    }
}

/// Navigation has already moved away from the editor. Tear down its immutable
/// snapshot and cancel late provider work without adding another history step.
pub fn leave() {
    if SAVE_ACTIVE.load(Ordering::Acquire) {
        // The file/DB transaction must finish, but work that can still publish
        // into the abandoned editor is cancelled immediately. The save task
        // clears the retained immutable snapshot once it completes.
        OPEN_GEN.fetch_add(1, Ordering::SeqCst);
        REMOTE_GEN.fetch_add(1, Ordering::SeqCst);
        ARTWORK_GEN.fetch_add(1, Ordering::SeqCst);
        return;
    }
    clear_session();
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct TrackDraft {
    id: String,
    title: String,
    track_number: String,
    disc_number: String,
    #[serde(default)]
    artist_credit: String,
    #[serde(default)]
    artists: Vec<String>,
    #[serde(default)]
    composers: Vec<String>,
    #[serde(default)]
    performers: Vec<String>,
    #[serde(default)]
    musicbrainz_recording_id: String,
    #[serde(default)]
    musicbrainz_track_id: String,
    #[serde(default)]
    musicbrainz_artist_ids: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SaveDraft {
    album_title: String,
    album_artist: String,
    #[serde(default)]
    album_artists: Vec<String>,
    #[serde(default)]
    compilation: bool,
    year: String,
    genre: String,
    catalog_number: String,
    persistence: String,
    id3v2_version: String,
    synchronize_secondary_tags: bool,
    #[serde(default)]
    musicbrainz_release_id: String,
    #[serde(default)]
    musicbrainz_release_group_id: String,
    #[serde(default)]
    musicbrainz_album_artist_ids: Vec<String>,
    #[serde(default)]
    discogs_release_id: String,
    #[serde(default)]
    artwork_token: String,
    tracks: Vec<TrackDraft>,
}

fn normalized_list(values: Vec<String>, field: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::<String>::new();
    let mut seen = HashSet::<String>::new();
    for value in values {
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        if value.chars().count() > 512 {
            return Err(qbz_i18n::t_args(
                "{} contains an excessively long value.",
                &[field],
            ));
        }
        let folded = value.to_lowercase();
        if seen.insert(folded) {
            out.push(value.to_string());
        }
        if out.len() > 64 {
            return Err(qbz_i18n::t_args("{} contains too many values.", &[field]));
        }
    }
    Ok(out)
}

fn normalized_id(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty() && value.len() <= 128).then(|| value.to_string())
}

fn parse_optional_number(value: &str, field: &str) -> Result<Option<u32>, String> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    match value.parse::<u32>() {
        Ok(number) if (1..=9999).contains(&number) => Ok(Some(number)),
        _ => Err(qbz_i18n::t_args("{} must be a positive number.", &[field])),
    }
}

fn parse_year(value: &str) -> Result<Option<u32>, String> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    match value.parse::<u32>() {
        Ok(year) if (1..=3000).contains(&year) => Ok(Some(year)),
        _ => Err(qbz_i18n::t("Year must be a valid number.")),
    }
}

struct SavePayload {
    session: EditorSession,
    album: AlbumTagWrite,
    direct_tracks: Vec<TrackTagWrite>,
    db_updates: Vec<TrackMetadataUpdateFull>,
    extended_album: ExtendedAlbumTagWrite,
    extended_tracks: Vec<ExtendedTrackTagWrite>,
    front_cover: Option<FrontCoverWrite>,
    sidecar: AlbumTagSidecar,
    direct: bool,
    options: DirectTagWriteOptions,
}

fn validate_draft(draft: SaveDraft, open: EditorSession) -> Result<SavePayload, String> {
    let album_title = draft.album_title.trim().to_string();
    if album_title.is_empty() {
        return Err(qbz_i18n::t("Album title is required."));
    }
    let year = parse_year(&draft.year)?;
    let direct = draft.persistence == "direct";
    let sacd = open
        .tracks
        .iter()
        .any(|track| qbz_disc::SacdRef::is_sacd_path(&track.file_path));
    if direct && sacd {
        return Err(qbz_i18n::t("SACD images use sidecar metadata."));
    }
    if !matches!(draft.persistence.as_str(), "sidecar" | "direct") {
        return Err(qbz_i18n::t("Unknown metadata persistence mode."));
    }
    if direct && matches!(&open.target, EditorTarget::Remote { .. }) {
        return Err(qbz_i18n::t(
            "Media-server metadata can only be edited with a sidecar.",
        ));
    }
    if direct
        && open
            .tracks
            .iter()
            .any(|track| track.cue_file_path.is_some() || track.cue_start_secs.is_some())
    {
        return Err(qbz_i18n::t(
            "CUE-based albums can only be edited with a sidecar.",
        ));
    }

    let mut incoming = HashMap::<i64, TrackDraft>::new();
    for row in draft.tracks {
        let id = row
            .id
            .parse::<i64>()
            .map_err(|_| qbz_i18n::t("The track list changed; reopen the editor."))?;
        if incoming.insert(id, row).is_some() {
            return Err(qbz_i18n::t("The track list contains a duplicate row."));
        }
    }
    let expected_ids = open
        .tracks
        .iter()
        .map(|track| track.id)
        .collect::<HashSet<_>>();
    if incoming.len() != expected_ids.len() || incoming.keys().any(|id| !expected_ids.contains(id))
    {
        return Err(qbz_i18n::t("The track list changed; reopen the editor."));
    }

    let album_artist = draft.album_artist.trim().to_string();
    let genre = draft.genre.trim().to_string();
    let catalog = draft.catalog_number.trim().to_string();
    let mut album_artists = normalized_list(draft.album_artists, &qbz_i18n::t("Album artists"))?;
    if album_artists.is_empty() && !album_artist.is_empty() {
        album_artists.push(album_artist.clone());
    }
    let front_cover = if draft.artwork_token.trim().is_empty() {
        None
    } else {
        let staged = open
            .staged_artwork
            .as_ref()
            .filter(|artwork| artwork.token == draft.artwork_token)
            .ok_or_else(|| qbz_i18n::t("The selected artwork changed; choose it again."))?;
        Some(FrontCoverWrite {
            bytes: staged.bytes.clone(),
        })
    };
    let extended_album = ExtendedAlbumTagWrite {
        album_artists: album_artists.clone(),
        compilation: Some(draft.compilation),
        musicbrainz_release_id: normalized_id(&draft.musicbrainz_release_id),
        musicbrainz_release_group_id: normalized_id(&draft.musicbrainz_release_group_id),
        musicbrainz_album_artist_ids: normalized_list(
            draft.musicbrainz_album_artist_ids,
            &qbz_i18n::t("MusicBrainz album artist IDs"),
        )?,
        discogs_release_id: normalized_id(&draft.discogs_release_id),
    };
    let mut direct_tracks = Vec::with_capacity(open.tracks.len());
    let mut db_updates = Vec::with_capacity(open.tracks.len());
    let mut sidecar_tracks = Vec::with_capacity(open.tracks.len());
    let mut extended_tracks = Vec::with_capacity(open.tracks.len());
    let mut extended_sidecar_tracks = Vec::with_capacity(open.tracks.len());
    for track in &open.tracks {
        let row = incoming
            .remove(&track.id)
            .expect("validated tag editor row set");
        let title = row.title.trim().to_string();
        if title.is_empty() {
            return Err(qbz_i18n::t("Every track needs a title."));
        }
        let track_number = parse_optional_number(&row.track_number, &qbz_i18n::t("Track number"))?;
        if sacd && track_number != track.track_number {
            return Err(qbz_i18n::t("SACD track numbers cannot be changed."));
        }
        let disc_number = parse_optional_number(&row.disc_number, &qbz_i18n::t("Disc number"))?;
        let artist_credit = if row.artist_credit.trim().is_empty() {
            track.artist.clone()
        } else {
            row.artist_credit.trim().to_string()
        };
        let mut artists = normalized_list(row.artists, &qbz_i18n::t("Artists"))?;
        if artists.is_empty() {
            artists.push(artist_credit.clone());
        }
        let composers = normalized_list(row.composers, &qbz_i18n::t("Composers"))?;
        let performers = normalized_list(row.performers, &qbz_i18n::t("Performers"))?;
        let musicbrainz_artist_ids = normalized_list(
            row.musicbrainz_artist_ids,
            &qbz_i18n::t("MusicBrainz artist IDs"),
        )?;
        direct_tracks.push(TrackTagWrite {
            file_path: track.file_path.clone(),
            title: title.clone(),
            track_number,
            disc_number,
        });
        db_updates.push(TrackMetadataUpdateFull {
            id: track.id,
            title: title.clone(),
            artist: artist_credit.clone(),
            album: album_title.clone(),
            album_artist: (!album_artist.is_empty()).then(|| album_artist.clone()),
            album_group_title: album_title.clone(),
            track_number,
            disc_number,
            year,
            genre: (!genre.is_empty()).then(|| genre.clone()),
            catalog_number: (!catalog.is_empty()).then(|| catalog.clone()),
        });
        extended_tracks.push(ExtendedTrackTagWrite {
            file_path: track.file_path.clone(),
            artist_credit: artist_credit.clone(),
            artists: artists.clone(),
            composers: composers.clone(),
            performers: performers.clone(),
            musicbrainz_recording_id: normalized_id(&row.musicbrainz_recording_id),
            musicbrainz_track_id: normalized_id(&row.musicbrainz_track_id),
            musicbrainz_artist_ids: musicbrainz_artist_ids.clone(),
        });
        extended_sidecar_tracks.push(TrackExtendedMetadataOverride {
            file_path: track.file_path.clone(),
            cue_start_secs: track.cue_start_secs,
            artist_credit,
            artists,
            composers,
            performers,
            musicbrainz_recording_id: normalized_id(&row.musicbrainz_recording_id),
            musicbrainz_track_id: normalized_id(&row.musicbrainz_track_id),
            musicbrainz_artist_ids,
        });
        // Sidecar v1 uses explicit sentinels for clears. Empty/None used to
        // mean "no override", which made a cleared field reappear on rescan.
        sidecar_tracks.push(TrackMetadataOverride {
            file_path: track.file_path.clone(),
            cue_start_secs: track.cue_start_secs,
            title: Some(title),
            track_number: Some(track_number.unwrap_or(0)),
            disc_number: Some(disc_number.unwrap_or(0)),
        });
    }

    let sidecar = AlbumTagSidecar::new(
        AlbumMetadataOverride {
            album_title: Some(album_title.clone()),
            album_artist: Some(album_artist.clone()),
            year: Some(year.unwrap_or(0)),
            genre: Some(genre.clone()),
            catalog_number: Some(catalog.clone()),
        },
        sidecar_tracks,
    )
    .with_extended(
        AlbumExtendedMetadataOverride {
            album_artists,
            compilation: Some(draft.compilation),
            musicbrainz_release_id: extended_album.musicbrainz_release_id.clone(),
            musicbrainz_release_group_id: extended_album.musicbrainz_release_group_id.clone(),
            musicbrainz_album_artist_ids: extended_album.musicbrainz_album_artist_ids.clone(),
            discogs_release_id: extended_album.discogs_release_id.clone(),
            artwork_path: None,
        },
        extended_sidecar_tracks,
    );
    let album = AlbumTagWrite {
        album_title,
        album_artist,
        year,
        genre: (!genre.is_empty()).then_some(genre),
        catalog_number: (!catalog.is_empty()).then_some(catalog),
    };
    let options = DirectTagWriteOptions {
        id3v2_version: if draft.id3v2_version == "2.3" {
            Id3v2WriteVersion::V23
        } else {
            Id3v2WriteVersion::V24
        },
        synchronize_secondary_tags: draft.synchronize_secondary_tags,
    };
    Ok(SavePayload {
        session: open,
        album,
        direct_tracks,
        db_updates,
        extended_album,
        extended_tracks,
        front_cover,
        sidecar,
        direct,
        options,
    })
}

fn apply_database_update(
    payload: &SavePayload,
    artwork_path: Option<&str>,
) -> Result<Vec<LocalTrack>, qbz_library::LibraryError> {
    let path = crate::local_state::db_path().ok_or_else(|| {
        qbz_library::LibraryError::Database("library path unavailable".to_string())
    })?;
    let mut db = qbz_library::LibraryDatabase::open(&path)?;
    db.update_tracks_metadata_and_artwork_by_id(&payload.db_updates, artwork_path)?;
    db.get_album_tracks(&payload.session.group_key)
}

fn updated_ephemeral_tracks(payload: &SavePayload) -> Vec<LocalTrack> {
    let updates = payload
        .db_updates
        .iter()
        .map(|update| (update.id, update))
        .collect::<HashMap<_, _>>();
    payload
        .session
        .tracks
        .iter()
        .cloned()
        .map(|mut track| {
            let update = updates
                .get(&track.id)
                .expect("validated ephemeral metadata row set");
            track.title = update.title.clone();
            track.track_number = update.track_number;
            track.disc_number = update.disc_number;
            track.album = payload.album.album_title.clone();
            track.album_group_title = payload.album.album_title.clone();
            track.album_artist = (!payload.album.album_artist.trim().is_empty())
                .then(|| payload.album.album_artist.clone());
            track.artist = update.artist.clone();
            track.year = payload.album.year;
            track.genre = payload.album.genre.clone();
            track.catalog_number = payload.album.catalog_number.clone();
            track
        })
        .collect()
}

enum SaveOutcome {
    Library {
        album_id: String,
        tracks: Vec<LocalTrack>,
    },
    Ephemeral,
    Remote,
}

pub fn save(draft_json: &str) {
    if SAVE_ACTIVE
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    let parsed = serde_json::from_str::<SaveDraft>(draft_json)
        .map_err(|_| qbz_i18n::t("The metadata form could not be read."));
    let open = session().lock().expect("tag editor session lock").clone();
    let payload = match parsed.and_then(|draft| {
        open.ok_or_else(|| qbz_i18n::t("The metadata editor is no longer open."))
            .and_then(|open| validate_draft(draft, open))
    }) {
        Ok(payload) => payload,
        Err(error) => {
            SAVE_ACTIVE.store(false, Ordering::Release);
            crate::toast_qt::error(error);
            return;
        }
    };
    let save_generation = SAVE_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    let open_generation = payload.session.generation;
    crate::tag_editor_bridge::ui(|mut bridge| {
        bridge.as_mut().set_editor_saving(true);
        bridge.as_mut().set_editor_progress_current(0);
        bridge.as_mut().set_editor_progress_total(0);
    });

    crate::spawn(async move {
        let result = tokio::task::spawn_blocking(move || {
            let mut payload = payload;
            let mut artwork_path = None::<String>;
            let remote_target = match &payload.session.target {
                EditorTarget::Remote { target } => Some(target.clone()),
                _ => None,
            };
            if let Some(target) = remote_target.as_ref() {
                if payload.direct {
                    return Err(qbz_library::LibraryError::Other(qbz_i18n::t(
                        "Media-server metadata can only be edited with a sidecar.",
                    )));
                }
                if let Some(cover) = payload.front_cover.as_ref() {
                    artwork_path =
                        qbz_library::MetadataExtractor::cache_artwork_bytes(&cover.bytes);
                    if artwork_path.is_none() {
                        return Err(qbz_library::LibraryError::Other(
                            "edited artwork could not be projected into the thumbnail cache"
                                .to_string(),
                        ));
                    }
                    if let Some(extended) = payload.sidecar.extended_album.as_mut() {
                        extended.artwork_path = artwork_path.clone();
                    }
                }
                crate::remote_metadata_qt::save(target, &payload.sidecar)
                    .map_err(qbz_library::LibraryError::Other)?;
            } else if payload.direct {
                let total = payload.direct_tracks.len() as i32;
                crate::tag_editor_bridge::ui(move |mut bridge| {
                    bridge.as_mut().set_editor_progress_total(total);
                });
                qbz_library::write_album_tags_to_files_extended(
                    &payload.album,
                    &payload.extended_album,
                    &payload.direct_tracks,
                    &payload.extended_tracks,
                    payload.front_cover.as_ref(),
                    payload.options,
                    |current, total| {
                        crate::tag_editor_bridge::ui(move |mut bridge| {
                            bridge.as_mut().set_editor_progress_current(current as i32);
                            bridge.as_mut().set_editor_progress_total(total as i32);
                        });
                    },
                )?;
                if let Some(cover) = payload.front_cover.as_ref() {
                    artwork_path =
                        qbz_library::MetadataExtractor::cache_artwork_bytes(&cover.bytes);
                    if artwork_path.is_none() {
                        return Err(qbz_library::LibraryError::Other(
                            "edited artwork could not be projected into the thumbnail cache"
                                .to_string(),
                        ));
                    }
                }
                // A sidecar is removed only after every file was verified.
                qbz_library::delete_album_sidecar(Path::new(&payload.session.directory))?;
            } else {
                let sacd_image = payload
                    .session
                    .tracks
                    .first()
                    .and_then(|track| qbz_disc::SacdRef::parse(&track.file_path))
                    .map(|reference| reference.image);
                let mut persistent_cover = sacd_image
                    .as_ref()
                    .and_then(|image| qbz_library::read_sacd_sidecar(image).ok().flatten())
                    .and_then(|sidecar| sidecar.extended_album)
                    .and_then(|album| album.artwork_path);
                if let Some(cover) = payload.front_cover.as_ref() {
                    if let Some(image) = sacd_image.as_ref() {
                        persistent_cover =
                            Some(qbz_library::write_sacd_front_cover(image, &cover.bytes)?);
                    } else {
                        qbz_library::write_folder_front_cover(
                            Path::new(&payload.session.directory),
                            &cover.bytes,
                        )?;
                    }
                    artwork_path =
                        qbz_library::MetadataExtractor::cache_artwork_bytes(&cover.bytes);
                    if artwork_path.is_none() {
                        return Err(qbz_library::LibraryError::Other(
                            "edited artwork could not be projected into the thumbnail cache"
                                .to_string(),
                        ));
                    }
                    if let Some(extended) = payload.sidecar.extended_album.as_mut() {
                        extended.artwork_path =
                            persistent_cover.clone().or_else(|| artwork_path.clone());
                    }
                }
                if sacd_image.is_some() {
                    if let Some(extended) = payload.sidecar.extended_album.as_mut() {
                        extended.artwork_path = persistent_cover;
                    }
                }
                if let Some(reference) = payload
                    .session
                    .tracks
                    .first()
                    .and_then(|track| qbz_disc::SacdRef::parse(&track.file_path))
                {
                    qbz_library::write_sacd_sidecar(&reference.image, &payload.sidecar)?;
                } else {
                    qbz_library::write_album_sidecar(
                        Path::new(&payload.session.directory),
                        &payload.sidecar,
                    )?;
                }
            }
            let outcome = match &payload.session.target {
                EditorTarget::Library => SaveOutcome::Library {
                    album_id: payload.session.album_id.clone(),
                    tracks: apply_database_update(&payload, artwork_path.as_deref())?,
                },
                EditorTarget::Ephemeral { session_path } => {
                    let mut tracks = updated_ephemeral_tracks(&payload);
                    if let Some(path) = artwork_path.as_ref() {
                        for track in &mut tracks {
                            track.artwork_path = Some(path.clone());
                        }
                    }
                    crate::local_ephemeral::apply_editor_update(session_path, &tracks)?;
                    SaveOutcome::Ephemeral
                }
                EditorTarget::Remote { .. } => SaveOutcome::Remote,
            };
            Ok::<_, qbz_library::LibraryError>(outcome)
        })
        .await
        .unwrap_or_else(|error| {
            Err(qbz_library::LibraryError::Other(format!(
                "metadata save task failed: {error}"
            )))
        });

        SAVE_ACTIVE.store(false, Ordering::Release);
        let editor_visible = crate::nav_qt::current_view() == "metadataeditor"
            || TRACK_MODAL_ACTIVE.load(Ordering::Acquire);
        if SAVE_GEN.load(Ordering::SeqCst) != save_generation {
            if !editor_visible {
                clear_session();
            }
            return;
        }
        if editor_visible {
            crate::tag_editor_bridge::ui(|mut bridge| {
                bridge.as_mut().set_editor_saving(false);
                bridge.as_mut().set_editor_progress_current(0);
                bridge.as_mut().set_editor_progress_total(0);
            });
        }
        match result {
            Ok(SaveOutcome::Library { album_id, tracks }) => {
                // Publish from the authoritative DB rows directly. In metadata
                // grouping the logical id may change as part of this edit, so
                // re-querying by the old id would close a successful save.
                if let Some(doc) = crate::local_album_actions::open_versions(&album_id, tracks) {
                    let json = crate::local_rows::to_json(&doc);
                    crate::local_bridge::ui(move |mut bridge| {
                        bridge
                            .as_mut()
                            .set_local_album_json(QString::from(json.as_str()));
                        bridge.as_mut().set_local_album_loading(false);
                    });
                }
                crate::local_bridge_ops::reload_browse();
                if OPEN_GEN.load(Ordering::SeqCst) == open_generation {
                    close();
                }
                crate::toast_qt::success(qbz_i18n::t("Album metadata saved."));
            }
            Ok(SaveOutcome::Ephemeral) => {
                if OPEN_GEN.load(Ordering::SeqCst) == open_generation {
                    close();
                }
                crate::toast_qt::success(qbz_i18n::t("Album metadata saved."));
            }
            Ok(SaveOutcome::Remote) => {
                // The remote caches remain authoritative and untouched. Their
                // derived catalog projection notices the sidecar revision and
                // rebuilds the affected source; direct mappers already see the
                // refreshed in-process overlay immediately.
                crate::local_catalog_qt::request_catch_up();
                crate::local_bridge_ops::reload_browse();
                if OPEN_GEN.load(Ordering::SeqCst) == open_generation {
                    close();
                }
                crate::toast_qt::success(qbz_i18n::t("Album metadata saved."));
            }
            Err(error) => {
                log::error!("[qbz-qt] metadata save failed: {error}");
                crate::toast_qt::error(qbz_i18n::t_args(
                    "Couldn't save metadata: {}",
                    &[&error.to_string()],
                ));
            }
        }
        if !editor_visible {
            clear_session();
        }
    });
}

fn publish_remote(kind: &str, value: serde_json::Value) {
    let sequence = REMOTE_SEQ.fetch_add(1, Ordering::Relaxed).wrapping_add(1);
    let json = serde_json::json!({ "kind": kind, "value": value }).to_string();
    crate::tag_editor_bridge::ui(move |mut bridge| {
        bridge
            .as_mut()
            .set_remote_json(QString::from(json.as_str()));
        bridge.as_mut().set_remote_seq(sequence);
    });
}

const MAX_ARTWORK_BYTES: usize = 25 * 1024 * 1024;

fn staged_artwork_seed(artwork: &StagedArtwork) -> ArtworkSeed {
    ArtworkSeed {
        preview_path: crate::artwork_qt::file_url(&artwork.preview_path),
        token: artwork.token.clone(),
        source: artwork.source.clone(),
        width: artwork.width,
        height: artwork.height,
    }
}

fn is_editor_stage_path(path: &str) -> bool {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with(".tag-editor-stage-"))
}

fn prepare_staged_artwork(
    generation: u64,
    bytes: Vec<u8>,
    source: String,
) -> Result<StagedArtwork, String> {
    if bytes.is_empty() || bytes.len() > MAX_ARTWORK_BYTES {
        return Err(qbz_i18n::t(
            "Artwork must be a non-empty image no larger than 25 MiB.",
        ));
    }
    let decoded = image::load_from_memory(&bytes)
        .map_err(|_| qbz_i18n::t("The selected file is not a supported image."))?;
    let width = decoded.width();
    let height = decoded.height();
    if width == 0 || height == 0 || width > 12_000 || height > 12_000 {
        return Err(qbz_i18n::t(
            "The selected artwork has unsupported dimensions.",
        ));
    }
    let sequence = ARTWORK_SEQ.fetch_add(1, Ordering::Relaxed) + 1;
    let directory = std::env::temp_dir().join("qbz-tag-editor");
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("{}: {error}", qbz_i18n::t("Couldn't stage artwork")))?;
    let path = directory.join(format!(".tag-editor-stage-{generation}-{sequence}.img"));
    std::fs::write(&path, &bytes)
        .map_err(|error| format!("{}: {error}", qbz_i18n::t("Couldn't stage artwork")))?;
    Ok(StagedArtwork {
        token: format!("artwork-{generation}-{sequence}"),
        bytes,
        preview_path: path.to_string_lossy().to_string(),
        source,
        width,
        height,
    })
}

fn install_staged_artwork(generation: u64, artwork: StagedArtwork) -> bool {
    let seed = staged_artwork_seed(&artwork);
    let prior = {
        let mut guard = session().lock().expect("tag editor session lock");
        let Some(open) = guard.as_mut().filter(|open| open.generation == generation) else {
            let _ = std::fs::remove_file(&artwork.preview_path);
            return false;
        };
        open.staged_artwork.replace(artwork)
    };
    if let Some(path) = prior
        .map(|artwork| artwork.preview_path)
        .filter(|path| is_editor_stage_path(path))
    {
        let _ = std::fs::remove_file(path);
    }
    publish_remote(
        "artwork-selected",
        serde_json::to_value(seed).unwrap_or_else(|_| serde_json::json!({})),
    );
    true
}

async fn download_bounded_https(url: &str) -> Result<Vec<u8>, String> {
    if !url.starts_with("https://") {
        return Err(qbz_i18n::t("Artwork source did not provide a secure URL."));
    }
    let client = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(8))
        .timeout(std::time::Duration::from_secs(30))
        .user_agent("QBZ/2 metadata-editor")
        .build()
        .map_err(|error| error.to_string())?;
    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(|error| error.to_string())?
        .error_for_status()
        .map_err(|error| error.to_string())?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_ARTWORK_BYTES as u64)
    {
        return Err(qbz_i18n::t("Artwork is larger than 25 MiB."));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|error| error.to_string())? {
        if bytes.len().saturating_add(chunk.len()) > MAX_ARTWORK_BYTES {
            return Err(qbz_i18n::t("Artwork is larger than 25 MiB."));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

pub fn choose_artwork() {
    let generation = session()
        .lock()
        .expect("tag editor session lock")
        .as_ref()
        .map(|open| open.generation);
    let Some(generation) = generation else {
        return;
    };
    crate::spawn(async move {
        let Some(file) = rfd::AsyncFileDialog::new()
            .set_title(&qbz_i18n::t("Choose album artwork"))
            .add_filter("Images", &["jpg", "jpeg", "png", "webp", "gif", "bmp"])
            .pick_file()
            .await
        else {
            return;
        };
        crate::tag_editor_bridge::ui(|mut bridge| {
            bridge.as_mut().set_artwork_loading(true);
        });
        let result = match tokio::fs::metadata(file.path()).await {
            Ok(metadata) if metadata.len() <= MAX_ARTWORK_BYTES as u64 => {
                match tokio::fs::read(file.path()).await {
                    Ok(bytes) => tokio::task::spawn_blocking(move || {
                        prepare_staged_artwork(generation, bytes, qbz_i18n::t("Local file"))
                    })
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|result| result),
                    Err(error) => Err(error.to_string()),
                }
            }
            Ok(_) => Err(qbz_i18n::t("Artwork is larger than 25 MiB.")),
            Err(error) => Err(error.to_string()),
        };
        crate::tag_editor_bridge::ui(|mut bridge| {
            bridge.as_mut().set_artwork_loading(false);
        });
        match result {
            Ok(artwork) => {
                install_staged_artwork(generation, artwork);
            }
            Err(error) => crate::toast_qt::error(error),
        }
    });
}

pub fn search_artwork(provider: &str, title: &str, artist: &str, catalog_number: &str) {
    let provider = provider.trim().to_ascii_lowercase();
    let title = title.trim().to_string();
    let artist = artist.trim().to_string();
    let catalog_number = catalog_number.trim().to_string();
    if title.is_empty() {
        crate::toast_qt::error(qbz_i18n::t("Enter an album title to find artwork."));
        return;
    }
    let open_generation = session()
        .lock()
        .expect("tag editor session lock")
        .as_ref()
        .map(|open| open.generation);
    let Some(open_generation) = open_generation else {
        return;
    };
    let operation = ARTWORK_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    crate::tag_editor_bridge::ui(|mut bridge| {
        bridge.as_mut().set_artwork_searching(true);
    });
    crate::spawn(async move {
        let result: Result<Vec<ArtworkCandidate>, String> = match provider.as_str() {
            "discogs" => qbz_integrations::DiscogsClient::new()
                .search_artwork_options(
                    &artist,
                    &title,
                    (!catalog_number.is_empty()).then_some(catalog_number.as_str()),
                )
                .await
                .map(|rows| {
                    rows.into_iter()
                        .enumerate()
                        .map(|(index, row)| ArtworkCandidate {
                            id: format!("art-{operation}-{index}"),
                            preview_url: row.url,
                            source: "Discogs".to_string(),
                            title: row.release_title.unwrap_or_else(|| title.clone()),
                            detail: format!(
                                "{} x {}{}",
                                row.width,
                                row.height,
                                row.release_year
                                    .map(|year| format!(" - {year}"))
                                    .unwrap_or_default()
                            ),
                        })
                        .collect()
                }),
            "lastfm" => qbz_integrations::LastFmClient::new()
                .get_album_info(&artist, &title)
                .await
                .map_err(|error| error.to_string())
                .map(|album| {
                    album
                        .image
                        .into_iter()
                        .enumerate()
                        .map(|(index, url)| ArtworkCandidate {
                            id: format!("art-{operation}-{index}"),
                            preview_url: url,
                            source: "Last.fm".to_string(),
                            title: album.name.clone(),
                            detail: album.artist.clone(),
                        })
                        .collect()
                }),
            _ => qbz_integrations::MusicBrainzClient::new()
                .search_releases_extended(
                    &title,
                    &artist,
                    (!catalog_number.is_empty()).then_some(catalog_number.as_str()),
                    10,
                )
                .await
                .map_err(|error| error.to_string())
                .map(|response| {
                    response
                        .releases
                        .into_iter()
                        .enumerate()
                        .map(|(index, release)| ArtworkCandidate {
                            id: format!("art-{operation}-{index}"),
                            preview_url: format!(
                                "https://coverartarchive.org/release/{}/front-500",
                                release.id
                            ),
                            source: "MusicBrainz".to_string(),
                            title: release.title,
                            detail: release.date.unwrap_or_default(),
                        })
                        .collect()
                }),
        };
        if ARTWORK_GEN.load(Ordering::SeqCst) != operation
            || OPEN_GEN.load(Ordering::SeqCst) != open_generation
        {
            return;
        }
        crate::tag_editor_bridge::ui(|mut bridge| {
            bridge.as_mut().set_artwork_searching(false);
        });
        match result {
            Ok(rows) => {
                let value = serde_json::to_value(&rows).unwrap_or_else(|_| serde_json::json!([]));
                let mut guard = session().lock().expect("tag editor session lock");
                if let Some(open) = guard
                    .as_mut()
                    .filter(|open| open.generation == open_generation)
                {
                    open.artwork_candidates = rows
                        .into_iter()
                        .map(|candidate| (candidate.id.clone(), candidate))
                        .collect();
                    drop(guard);
                    publish_remote("artwork-results", value);
                }
            }
            Err(error) => {
                log::warn!("[qbz-qt] artwork search failed: {error}");
                publish_remote("artwork-results", serde_json::json!([]));
                crate::toast_qt::error(qbz_i18n::t("Artwork search failed."));
            }
        }
    });
}

pub fn select_artwork(candidate_id: &str) {
    let selected = {
        let guard = session().lock().expect("tag editor session lock");
        guard.as_ref().and_then(|open| {
            open.artwork_candidates
                .get(candidate_id)
                .cloned()
                .map(|candidate| (open.generation, candidate))
        })
    };
    let Some((open_generation, candidate)) = selected else {
        crate::toast_qt::error(qbz_i18n::t("That artwork result is no longer available."));
        return;
    };
    let operation = ARTWORK_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    crate::tag_editor_bridge::ui(|mut bridge| {
        bridge.as_mut().set_artwork_loading(true);
    });
    crate::spawn(async move {
        let bytes = if candidate.source == "Discogs" {
            qbz_integrations::DiscogsClient::new()
                .download_artwork_bytes(&candidate.preview_url, MAX_ARTWORK_BYTES)
                .await
        } else {
            download_bounded_https(&candidate.preview_url).await
        };
        let result = match bytes {
            Ok(bytes) => tokio::task::spawn_blocking(move || {
                prepare_staged_artwork(open_generation, bytes, candidate.source)
            })
            .await
            .map_err(|error| error.to_string())
            .and_then(|result| result),
            Err(error) => Err(error),
        };
        if ARTWORK_GEN.load(Ordering::SeqCst) != operation
            || OPEN_GEN.load(Ordering::SeqCst) != open_generation
        {
            if let Ok(artwork) = result {
                let _ = std::fs::remove_file(artwork.preview_path);
            }
            return;
        }
        crate::tag_editor_bridge::ui(|mut bridge| {
            bridge.as_mut().set_artwork_loading(false);
        });
        match result {
            Ok(artwork) => {
                install_staged_artwork(open_generation, artwork);
            }
            Err(error) => {
                log::warn!("[qbz-qt] artwork download failed: {error}");
                crate::toast_qt::error(qbz_i18n::t("Couldn't download that artwork."));
            }
        }
    });
}

pub fn clear_artwork() {
    ARTWORK_GEN.fetch_add(1, Ordering::SeqCst);
    let prior = session()
        .lock()
        .expect("tag editor session lock")
        .as_mut()
        .and_then(|open| open.staged_artwork.take());
    if let Some(path) = prior
        .map(|artwork| artwork.preview_path)
        .filter(|path| is_editor_stage_path(path))
    {
        let _ = std::fs::remove_file(path);
    }
    crate::tag_editor_bridge::ui(|mut bridge| {
        bridge.as_mut().set_artwork_loading(false);
        bridge.as_mut().set_artwork_searching(false);
    });
    publish_remote(
        "artwork-selected",
        serde_json::to_value(ArtworkSeed::default()).unwrap_or_else(|_| serde_json::json!({})),
    );
}

pub fn search_remote(provider: &str, title: &str, artist: &str) {
    let provider = provider.to_string();
    let title = title.trim().to_string();
    let artist = artist.trim().to_string();
    if title.is_empty() && artist.is_empty() {
        crate::toast_qt::error(qbz_i18n::t("Enter an album title or artist to search."));
        return;
    }
    let generation = REMOTE_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    crate::tag_editor_bridge::ui(|mut bridge| {
        bridge.as_mut().set_remote_searching(true);
        bridge.as_mut().set_remote_loading(false);
    });
    crate::spawn(async move {
        let result: Result<Vec<qbz_integrations::RemoteAlbumSearchResult>, String> =
            if provider == "discogs" {
                let client = qbz_integrations::DiscogsClient::new();
                client
                    .search_releases(&artist, &title, None, 12)
                    .await
                    .map(|rows| {
                        rows.iter()
                            .map(qbz_integrations::discogs_extended_to_search_result)
                            .collect()
                    })
            } else {
                let client = qbz_integrations::MusicBrainzClient::new();
                client
                    .search_releases_extended(&title, &artist, None, 12)
                    .await
                    .map(|response| {
                        response
                            .releases
                            .iter()
                            .map(qbz_integrations::musicbrainz_release_to_search_result)
                            .collect()
                    })
                    .map_err(|error| error.to_string())
            };
        if REMOTE_GEN.load(Ordering::SeqCst) != generation {
            return;
        }
        crate::tag_editor_bridge::ui(|mut bridge| {
            bridge.as_mut().set_remote_searching(false);
        });
        match result {
            Ok(rows) => publish_remote(
                "results",
                serde_json::to_value(rows).unwrap_or_else(|_| serde_json::json!([])),
            ),
            Err(error) => {
                log::warn!("[qbz-qt] metadata search failed: {error}");
                publish_remote("results", serde_json::json!([]));
                crate::toast_qt::error(qbz_i18n::t("Metadata search failed."));
            }
        }
    });
}

pub fn load_remote(provider: &str, provider_id: &str) {
    let provider = provider.to_string();
    let provider_id = provider_id.to_string();
    if provider_id.trim().is_empty() {
        return;
    }
    let generation = REMOTE_GEN.fetch_add(1, Ordering::SeqCst) + 1;
    crate::tag_editor_bridge::ui(|mut bridge| {
        bridge.as_mut().set_remote_loading(true);
    });
    crate::spawn(async move {
        let result: Result<qbz_integrations::RemoteAlbumMetadata, String> = if provider == "discogs"
        {
            match provider_id.parse::<u64>() {
                Ok(id) => {
                    let client = qbz_integrations::DiscogsClient::new();
                    client
                        .get_release_metadata(id)
                        .await
                        .map(|metadata| qbz_integrations::discogs_full_to_metadata(&metadata))
                }
                Err(_) => Err("invalid Discogs release id".to_string()),
            }
        } else {
            let client = qbz_integrations::MusicBrainzClient::new();
            client
                .get_release_with_tracks(&provider_id)
                .await
                .map(|release| qbz_integrations::musicbrainz_full_to_metadata(&release))
                .map_err(|error| error.to_string())
        };
        if REMOTE_GEN.load(Ordering::SeqCst) != generation {
            return;
        }
        crate::tag_editor_bridge::ui(|mut bridge| {
            bridge.as_mut().set_remote_loading(false);
        });
        match result {
            Ok(metadata) => publish_remote(
                "metadata",
                serde_json::to_value(metadata).unwrap_or_else(|_| serde_json::json!({})),
            ),
            Err(error) => {
                log::warn!("[qbz-qt] metadata result load failed: {error}");
                crate::toast_qt::error(qbz_i18n::t("Couldn't load that metadata result."));
            }
        }
    });
}

pub fn open_remote(provider: &str, provider_id: &str) {
    let url = if provider == "discogs" {
        format!("https://www.discogs.com/release/{provider_id}")
    } else {
        format!("https://musicbrainz.org/release/{provider_id}")
    };
    if open::that(url).is_err() {
        crate::toast_qt::error(qbz_i18n::t("Couldn't open the metadata source."));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session_fixture() -> EditorSession {
        EditorSession {
            generation: 7,
            album_id: "album:test".to_string(),
            group_key: "/library/album".to_string(),
            directory: "/library/album".to_string(),
            target: EditorTarget::Library,
            staged_artwork: None,
            artwork_candidates: HashMap::new(),
            tracks: vec![
                LocalTrack {
                    id: 10,
                    file_path: "/library/album/01.flac".to_string(),
                    title: "One".to_string(),
                    artist: "Artist".to_string(),
                    album_artist: Some("Artist".to_string()),
                    ..LocalTrack::default()
                },
                LocalTrack {
                    id: 20,
                    file_path: "/library/album/02.flac".to_string(),
                    title: "Two".to_string(),
                    artist: "Artist".to_string(),
                    album_artist: Some("Artist".to_string()),
                    ..LocalTrack::default()
                },
            ],
        }
    }

    fn draft_json(rows: &str) -> String {
        format!(
            r#"{{
                "albumTitle":"Album",
                "albumArtist":"Artist",
                "year":"2026",
                "genre":"Rock",
                "catalogNumber":"CAT-1",
                "persistence":"sidecar",
                "id3v2Version":"2.4",
                "synchronizeSecondaryTags":false,
                "tracks":{rows}
            }}"#
        )
    }

    #[test]
    fn optional_numbers_are_strict_and_blank_clears() {
        assert_eq!(parse_optional_number("", "Track").unwrap(), None);
        assert_eq!(parse_optional_number(" 12 ", "Track").unwrap(), Some(12));
        assert!(parse_optional_number("0", "Track").is_err());
        assert!(parse_optional_number("nope", "Track").is_err());
    }

    #[test]
    fn remote_album_ids_strip_each_providers_group_prefix() {
        assert_eq!(remote_album_id("plex", "plex:album:4812"), "4812");
        assert_eq!(remote_album_id("jellyfin", "jellyfin:album-9"), "album-9");
        assert_eq!(remote_album_id("subsonic", "subsonic:al-7"), "al-7");
        // An unprefixed key is already the provider id.
        assert_eq!(remote_album_id("jellyfin", "album-9"), "album-9");
    }

    #[test]
    fn editor_source_recovers_remote_rows_from_album_namespace() {
        let mut track = LocalTrack {
            source: None,
            album_group_key: "jellyfin:album-9".to_string(),
            file_path: "opaque-server-item".to_string(),
            ..LocalTrack::default()
        };
        assert_eq!(editor_remote_source(&track), "jellyfin");

        track.album_group_key = "navidrome:album-4".to_string();
        assert_eq!(editor_remote_source(&track), "subsonic");

        track.source = Some("plex".to_string());
        track.album_group_key.clear();
        assert_eq!(editor_remote_source(&track), "plex");
    }

    /// A fixture directory INSIDE the temp dir. `std::env::temp_dir()` itself
    /// ends in a separator on Windows (`C:\...\Temp\`) while `Path::parent`
    /// never yields one, so comparing the two strings failed the Windows gate
    /// on every run since 2026-08-28. Joining a component normalises both.
    fn fixture_directory(name: &str) -> std::path::PathBuf {
        let directory = std::env::temp_dir().join(name);
        std::fs::create_dir_all(&directory).expect("fixture directory");
        directory
    }

    #[test]
    fn sacd_editor_resolves_image_directory_and_refuses_direct_writes() {
        let mut open = session_fixture();
        let directory = fixture_directory("qbz-editor-sacd-fixture");
        let image = directory.join("Disc #1.iso");
        for (index, track) in open.tracks.iter_mut().enumerate() {
            track.file_path = qbz_disc::SacdRef {
                image: image.clone(),
                track: (index + 1) as u8,
            }
            .to_path_string();
            track.track_number = Some(index as u32 + 1);
            track.album_group_key = "sacd|||fingerprint".into();
        }
        open.directory = directory.to_string_lossy().into_owned();
        assert_eq!(
            editor_directory(&open.tracks[0]),
            Some(open.directory.clone())
        );
        let seed = build_seed(&open);
        assert!(!seed.can_direct_write);
        assert!(seed.fixed_track_numbers);
        assert!(seed.inspection.error.is_empty());
        let rows = r#"[{"id":"10","title":"One","trackNumber":"1","discNumber":"1"},{"id":"20","title":"Two","trackNumber":"2","discNumber":"1"}]"#;
        let draft: SaveDraft = serde_json::from_str(&draft_json(rows)).unwrap();
        assert!(validate_draft(draft, open.clone()).is_ok());
        let mut direct: SaveDraft = serde_json::from_str(&draft_json(rows)).unwrap();
        direct.persistence = "direct".into();
        assert!(validate_draft(direct, open.clone()).is_err());
        let mut renumbered: SaveDraft = serde_json::from_str(&draft_json(rows)).unwrap();
        renumbered.tracks[0].track_number = "2".into();
        assert!(validate_draft(renumbered, open).is_err());
    }

    #[test]
    fn editor_directory_uses_real_parent_for_non_path_group_identity() {
        let directory = fixture_directory("qbz-editor-location-fixture");
        let file = directory.join("qbz-editor-location-fixture.flac");
        let track = LocalTrack {
            file_path: file.to_string_lossy().into_owned(),
            album_group_key: "metadata:artist|album".to_string(),
            ..LocalTrack::default()
        };
        assert_eq!(
            editor_directory(&track).as_deref(),
            Some(directory.to_string_lossy().as_ref())
        );
    }

    #[test]
    fn year_validation_does_not_accept_sentinels_from_qml() {
        assert_eq!(parse_year("").unwrap(), None);
        assert_eq!(parse_year("2026").unwrap(), Some(2026));
        assert!(parse_year("0").is_err());
        assert!(parse_year("3001").is_err());
    }

    #[test]
    fn draft_ids_resolve_only_to_the_snapshotted_paths() {
        let draft: SaveDraft = serde_json::from_str(&draft_json(
            r#"[
                {"id":"20","title":"Second","trackNumber":"2","discNumber":"1"},
                {"id":"10","title":"First","trackNumber":"1","discNumber":"1"}
            ]"#,
        ))
        .unwrap();

        let payload = validate_draft(draft, session_fixture()).unwrap();
        assert_eq!(payload.direct_tracks[0].file_path, "/library/album/01.flac");
        assert_eq!(payload.direct_tracks[0].title, "First");
        assert_eq!(payload.direct_tracks[1].file_path, "/library/album/02.flac");
        assert_eq!(payload.direct_tracks[1].title, "Second");
    }

    #[test]
    fn ephemeral_rows_receive_album_and_track_edits_without_changing_paths() {
        let draft: SaveDraft = serde_json::from_str(&draft_json(
            r#"[
                {"id":"10","title":"First","trackNumber":"3","discNumber":"2"},
                {"id":"20","title":"Second","trackNumber":"4","discNumber":"2"}
            ]"#,
        ))
        .unwrap();
        let payload = validate_draft(draft, session_fixture()).unwrap();
        let tracks = updated_ephemeral_tracks(&payload);

        assert_eq!(tracks[0].file_path, "/library/album/01.flac");
        assert_eq!(tracks[0].album, "Album");
        assert_eq!(tracks[0].album_artist.as_deref(), Some("Artist"));
        assert_eq!(tracks[0].title, "First");
        assert_eq!(tracks[0].track_number, Some(3));
        assert_eq!(tracks[0].disc_number, Some(2));
        assert_eq!(tracks[0].genre.as_deref(), Some("Rock"));
    }

    #[test]
    fn injected_rows_or_paths_are_rejected() {
        let injected_id: SaveDraft = serde_json::from_str(&draft_json(
            r#"[
                {"id":"10","title":"First","trackNumber":"1","discNumber":"1"},
                {"id":"999","title":"Injected","trackNumber":"2","discNumber":"1"}
            ]"#,
        ))
        .unwrap();
        assert!(validate_draft(injected_id, session_fixture()).is_err());

        let injected_path = draft_json(
            r#"[
                {"id":"10","filePath":"/tmp/other.flac","title":"First","trackNumber":"1","discNumber":"1"},
                {"id":"20","title":"Second","trackNumber":"2","discNumber":"1"}
            ]"#,
        );
        assert!(serde_json::from_str::<SaveDraft>(&injected_path).is_err());
    }

    #[test]
    fn rich_credit_components_and_provider_ids_survive_validation() {
        let draft: SaveDraft = serde_json::from_str(
            r#"{
                "albumTitle":"Compilation",
                "albumArtist":"Various Artists",
                "albumArtists":["Various Artists"],
                "compilation":true,
                "year":"2026",
                "genre":"Soundtrack",
                "catalogNumber":"CAT-2",
                "persistence":"sidecar",
                "id3v2Version":"2.4",
                "synchronizeSecondaryTags":false,
                "musicbrainzReleaseId":"release",
                "musicbrainzReleaseGroupId":"group",
                "musicbrainzAlbumArtistIds":["va-id"],
                "discogsReleaseId":"123",
                "tracks":[
                    {"id":"10","title":"First","trackNumber":"1","discNumber":"1",
                     "artistCredit":"Alpha feat. Beta","artists":["Alpha","Beta"],
                     "composers":["Composer"],"performers":["Player (guitar)"],
                     "musicbrainzRecordingId":"recording-1","musicbrainzTrackId":"track-1",
                     "musicbrainzArtistIds":["alpha-id","beta-id"]},
                    {"id":"20","title":"Second","trackNumber":"2","discNumber":"1",
                     "artistCredit":"Gamma","artists":["Gamma"],
                     "musicbrainzRecordingId":"recording-2","musicbrainzTrackId":"track-2",
                     "musicbrainzArtistIds":["gamma-id"]}
                ]
            }"#,
        )
        .unwrap();

        let payload = validate_draft(draft, session_fixture()).unwrap();
        assert_eq!(payload.extended_album.album_artists, ["Various Artists"]);
        assert_eq!(payload.extended_album.compilation, Some(true));
        assert_eq!(
            payload
                .extended_album
                .musicbrainz_release_group_id
                .as_deref(),
            Some("group")
        );
        assert_eq!(payload.extended_tracks[0].artist_credit, "Alpha feat. Beta");
        assert_eq!(payload.extended_tracks[0].artists, ["Alpha", "Beta"]);
        assert_eq!(payload.db_updates[0].artist, "Alpha feat. Beta");
        assert_eq!(
            payload.sidecar.extended_tracks[0]
                .musicbrainz_recording_id
                .as_deref(),
            Some("recording-1")
        );
    }

    #[test]
    fn media_server_sessions_preserve_full_drafts_but_reject_direct_writes() {
        let mut remote = session_fixture();
        remote.directory.clear();
        remote.target = EditorTarget::Remote {
            target: qbz_library::RemoteTagTarget::new("jellyfin", "server-a", "album-9"),
        };
        for (index, track) in remote.tracks.iter_mut().enumerate() {
            track.file_path = format!("native-track-{}", index + 1);
            track.source = Some("jellyfin".to_string());
        }
        let rows = r#"[
            {"id":"10","title":"First","trackNumber":"1","discNumber":"1",
             "artistCredit":"Alpha","artists":["Alpha"],"composers":["Composer"],
             "performers":["Player"],"musicbrainzRecordingId":"recording-1",
             "musicbrainzTrackId":"track-1","musicbrainzArtistIds":["artist-1"]},
            {"id":"20","title":"Second","trackNumber":"2","discNumber":"1",
             "artistCredit":"Beta","artists":["Beta"]}
        ]"#;
        let sidecar: SaveDraft = serde_json::from_str(&draft_json(rows)).unwrap();
        let payload = validate_draft(sidecar, remote.clone()).unwrap();
        assert!(!payload.direct);
        assert_eq!(payload.sidecar.extended_tracks[0].composers, ["Composer"]);
        assert_eq!(payload.sidecar.tracks[0].file_path, "native-track-1");

        let direct_json =
            draft_json(rows).replace("\"persistence\":\"sidecar\"", "\"persistence\":\"direct\"");
        let direct: SaveDraft = serde_json::from_str(&direct_json).unwrap();
        assert!(validate_draft(direct, remote).is_err());
    }

    #[test]
    fn artwork_staging_rejects_non_images_and_uses_an_opaque_token() {
        assert!(prepare_staged_artwork(99, b"not an image".to_vec(), "test".into()).is_err());

        let mut bytes = Vec::new();
        image::DynamicImage::new_rgb8(2, 3)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        let staged = prepare_staged_artwork(99, bytes, "test".into()).unwrap();
        assert!(staged.token.starts_with("artwork-99-"));
        assert!(!staged.token.contains(&staged.preview_path));
        assert_eq!((staged.width, staged.height), (2, 3));
        std::fs::remove_file(staged.preview_path).unwrap();
    }

    #[test]
    fn view_consumes_seed_when_loading_finishes_after_json_publish() {
        let qml = include_str!("../qml/controls/TagEditorModal.qml");
        let workspace = include_str!("../qml/controls/TagEditorWorkspace.qml");
        assert!(qml.contains("function onEditorJsonChanged()"));
        assert!(qml.contains("function onEditorLoadingChanged()"));
        assert!(qml.contains("!QbzTagEditor.editorLoading"));
        assert!(qml.contains("event.kind === \"artwork-selected\""));
        assert!(workspace.contains("QbzTagEditor.chooseArtwork()"));
        assert!(workspace.contains("id: trackDelegate"));
        assert!(workspace.contains("workspace.editor.selectedTrackIndex = trackDelegate.index"));
        assert!(!workspace.contains("selectedTrackIndex = index"));
        assert!(qml.contains("TagEditorWorkspace"));
        assert!(workspace.contains("* 8 / 12"));
        assert!(workspace.contains("workspace.compactPane === \"tracks\""));
        assert!(workspace.contains("workspace.compactPane === \"tags\""));
        assert!(workspace.contains("color: selected ? theme.alphaTier(12)"));
        assert!(workspace
            .contains("visible: QbzTagEditor.artworkSearching || QbzTagEditor.artworkLoading"));
        assert!(workspace.contains("id: lookupCard"));
        // SettingsButton is the shared secondary settings control and has no
        // `primary` property. An unknown assignment invalidates the entire QML
        // type lazily, so neither AlbumView nor ephemeral can mount the editor.
        assert!(!workspace.contains("primary: true"));
    }

    #[test]
    fn track_editor_reuses_every_album_field_and_persistence_control() {
        let wrapper = include_str!("../qml/controls/TrackMetadataModal.qml");
        let shared = include_str!("../qml/controls/TagEditorModal.qml");
        let workspace = include_str!("../qml/controls/TagEditorWorkspace.qml");
        assert!(wrapper.contains("TagEditorModal"));
        assert!(wrapper.contains("trackMode: true"));
        assert!(wrapper.contains("leaveOnDestruction: false"));
        assert!(shared.contains("onClicked: root.selectedTrackIndex--"));
        assert!(shared.contains("onClicked: root.selectedTrackIndex++"));
        assert!(shared.contains("QbzTagEditor.promoteToAlbumEditor()"));
        assert!(shared.contains("root.persistence = index === 1 ? \"direct\" : \"sidecar\""));
        assert!(shared.contains("QbzTagEditor.save(draft())"));
        assert!(workspace.contains("workspace.editor.albumTitle"));
        assert!(workspace.contains("workspace.editor.musicbrainzReleaseId"));
        assert!(
            workspace.contains("workspace.editor.musicbrainzRecordingId")
                || workspace.contains("rowData.musicbrainzRecordingId")
        );
        assert!(!wrapper.contains("filePath"));
    }
}
