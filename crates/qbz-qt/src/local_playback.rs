//! Local Library playback: `LocalTrack` -> `QueueTrack` -> `core().set_queue`
//! -> the player's audible seam.
//!
//! Split out of `local_library_qt.rs` (phase-24 modularization). A queue built
//! here can mix LOCAL files, EPHEMERAL rows, OFFLINE (Qobuz download) copies
//! and PLEX rows.
//!
//! THE AUDIBLE STEP IS NO LONGER HERE (design 02 §9 stage 3). This file used
//! to own `play_local_file`, `play_plex_track` and a `plex_rating_key` ladder,
//! and `local_album_actions` + `local_ephemeral` each carried their own copy of
//! the same routine. All three are gone: `qbz_source::SourceRegistry::playback`
//! claims the row and answers with a `PlaybackTicket`, and `audible_qt`
//! performs it. What is left here is queue BUILDING — mapping rows, ordering
//! them, stamping context — plus `play_audible`, which is now one call.
//!
//! The PROTECTED audio path is entered in `audible_qt`, never modified: the
//! bytes go to the same `play_data` seam the Slint frontend uses, so sample
//! rate / bit depth stay whatever the decoder found.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use qbz_app::shell::AppRuntime;
use qbz_core::LoggingAdapter;
use qbz_library::LocalTrack;
use qbz_models::QueueTrack;
use qbz_source::PlaybackTicket;

use crate::local_albums::fetch_album_tracks_blocking;
use crate::local_state::{state, with_db};

type Runtime = Arc<AppRuntime<LoggingAdapter>>;

const LOCAL_FILE_UNAVAILABLE: &str = "File not available — is the drive mounted?";

fn is_probeable_local_queue_row(track: &QueueTrack) -> bool {
    matches!(
        track.source.as_deref(),
        Some("local" | "user" | "ephemeral")
    )
}

/// Prove that a physical local row can be opened before any caller changes
/// the queue cursor or publishes now-playing metadata. Local decoding starts
/// lazily, so discovering a moved file inside the audible step is too late:
/// the old audio can still be heard while queue, NPB and MPRIS already claim
/// the failed replacement is playing.
///
/// The bounded reachability probe preserves the NAS rule: timeout is
/// transient evidence only. It marks the row for this session and never
/// deletes or rewrites the authoritative library database.
pub(crate) async fn preflight_queue_track(track: &QueueTrack) -> Result<(), String> {
    if !is_probeable_local_queue_row(track) {
        return Ok(());
    }
    let ticket = match qbz_source::registry().playback(track).await {
        Ok(ticket) => ticket,
        Err(error) => {
            let detail = error.to_string();
            log::warn!(
                "[qbz-qt] local playback preflight: track {} could not resolve: {detail}",
                track.id
            );
            crate::local_bridge::emit_track_availability(
                "local",
                track.id,
                Some(LOCAL_FILE_UNAVAILABLE),
            );
            crate::toast_qt::warning(qbz_i18n::t(LOCAL_FILE_UNAVAILABLE));
            return Err(detail);
        }
    };
    let path = match ticket {
        PlaybackTicket::File { path, .. } | PlaybackTicket::DsdFile { path, .. } => path,
        PlaybackTicket::Bytes { .. }
        | PlaybackTicket::Stream { .. }
        | PlaybackTicket::Catalog { .. }
        | PlaybackTicket::SeekLoaded { .. }
        | PlaybackTicket::CdTrack { .. } => return Ok(()),
    };
    // SACD references deliberately ride the DSD ticket but are not filesystem
    // paths; their demuxer owns medium availability and must not be rejected by
    // a path probe that cannot understand the reference.
    if qbz_disc::SacdRef::is_sacd_path(&path.to_string_lossy()) {
        return Ok(());
    }
    let reach = tokio::task::spawn_blocking(move || qbz_library::probe_default(&path))
        .await
        .unwrap_or(qbz_library::Reach::Unreachable);
    if reach.is_playable() {
        crate::local_bridge::emit_track_availability("local", track.id, None);
        return Ok(());
    }
    let detail = match reach {
        qbz_library::Reach::Missing => "local file is missing",
        qbz_library::Reach::Unreachable => "local file is temporarily unreachable",
        qbz_library::Reach::Present => unreachable!(),
    };
    log::warn!(
        "[qbz-qt] local playback preflight: track {} {detail}",
        track.id
    );
    crate::local_bridge::emit_track_availability("local", track.id, Some(LOCAL_FILE_UNAVAILABLE));
    crate::toast_qt::warning(qbz_i18n::t(LOCAL_FILE_UNAVAILABLE));
    Err(detail.to_string())
}

// ---------------------------------------------------------------------------
// On-disk cover backfill (playback.rs `fill_missing_covers` +
// local_library.rs `find_folder_cover`, both 1:1)
// ---------------------------------------------------------------------------

/// The reference's robust on-disk cover lookup for one folder
/// (`local_library.rs:3110 find_folder_cover`): a known stem first, then a
/// file named after the folder, then any image at all. Stems and extensions
/// are the reference's list verbatim — do not prune it, the order IS the
/// preference.
pub(crate) fn find_folder_cover(folder: &Path) -> Option<String> {
    const STEMS: &[&str] = &[
        "cover",
        "folder",
        "front",
        "art",
        "album",
        "albumart",
        "albumartsmall",
        "thumb",
        "artwork",
        "scan",
        "booklet",
        "title",
    ];
    const EXTS: &[&str] = &["jpg", "jpeg", "png", "webp", "bmp", "gif", "tif", "tiff"];
    let is_img = |p: &Path| {
        p.extension()
            .and_then(|e| e.to_str())
            .map(|e| EXTS.iter().any(|x| x.eq_ignore_ascii_case(e)))
            .unwrap_or(false)
    };
    let mut entries: Vec<PathBuf> = std::fs::read_dir(folder)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && is_img(p))
        .collect();
    if entries.is_empty() {
        return None;
    }
    entries.sort();
    let stem_lower = |p: &Path| {
        p.file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_lowercase())
            .unwrap_or_default()
    };
    let folder_name = folder
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_lowercase();
    let by_stem = entries
        .iter()
        .find(|p| STEMS.contains(&stem_lower(p).as_str()));
    let by_name = entries
        .iter()
        .find(|p| !folder_name.is_empty() && stem_lower(p) == folder_name);
    by_stem
        .or(by_name)
        .cloned()
        .or_else(|| entries.into_iter().next())
        .map(|p| p.to_string_lossy().into_owned())
}

/// Fill `artwork_path` from a cover image sitting in the track's own folder —
/// the port of `playback.rs:1490 fill_missing_covers`, extended for box sets:
/// a folder-local cover replaces an album-root value stored by an older scan.
/// Current scans already resolve embedded/disc/collection art in that order;
/// this queue-time pass keeps pre-migration rows equally safe until rescan.
///
/// Rows with no folder-local cover keep their existing artwork and only then
/// use the reference fallback. Remote rows never touch the filesystem.
///
/// which the reference runs on EVERY local play/enqueue path before it maps
/// rows to `QueueTrack`s (playback.rs:1094/1322/1349/1382, local_library.rs:1826,
/// main.rs:10636, offline_favorites.rs:128). This port had none of it, so a
/// library row whose `artwork_path` is NULL — the offline cache writes
/// `cover.jpg` next to the file without always backfilling the index, and a
/// ripped folder often carries the art only on disk — reached the queue with
/// `artwork_url: None` and the panel had nothing to request.
///
/// BLOCKING (one `read_dir` per distinct folder, memoized), so every caller
/// runs it inside `spawn_blocking`: a NAS-backed library would otherwise stall
/// a tokio worker.
pub(crate) fn fill_missing_covers(tracks: &mut [LocalTrack]) {
    use std::collections::HashMap;
    let mut memo: HashMap<String, Option<String>> = HashMap::new();
    for t in tracks.iter_mut() {
        // Media-server `file_path` values are ids/URLs, not audio files whose
        // parent may be scanned. A Qobuz download is different: its path is
        // the encrypted bundle directory and that directory can legitimately
        // contain the only `cover.jpg`, so retain the old missing-art fallback
        // for it without overriding an already authoritative artwork path.
        if matches!(
            t.source.as_deref(),
            Some("plex" | "jellyfin" | "subsonic" | "navidrome" | "gonic" | "airsonic" | "astiga")
        ) {
            continue;
        }
        let p = Path::new(&t.file_path);
        let folder = if p.is_dir() {
            p.to_path_buf()
        } else {
            match p.parent() {
                Some(d) => d.to_path_buf(),
                None => continue,
            }
        };
        let key = folder.to_string_lossy().into_owned();
        let folder_cover = memo
            .entry(key)
            .or_insert_with(|| find_folder_cover(&folder))
            .clone();
        let may_override = t.source.as_deref() != Some("qobuz_download");
        let artwork_missing = t.artwork_path.as_deref().is_none_or(str::is_empty);
        if folder_cover.is_some() && (may_override || artwork_missing) {
            // Override is intentional: `artwork_path` can name Box/cover.jpg
            // while THIS row lives under Box/Disc 05/cover.jpg.
            t.artwork_path = folder_cover;
        }
    }
}

// ---------------------------------------------------------------------------
// Queue mapping (playback.rs `local_queue_track` 1:1, all three sources)
// ---------------------------------------------------------------------------

pub fn local_queue_track(t: &LocalTrack) -> QueueTrack {
    let src = match t.source.as_deref() {
        Some("qobuz_download") => "qobuz_download",
        Some("plex") => "plex",
        // The media servers. Folding these into "local" — which the `_` arm
        // did — put a Jellyfin row into the queue wearing the wrong source
        // word AND with no server id, so it changed the now-playing bar and
        // then played nothing: `claim` had a namespaced id and a word that
        // disagreed with it.
        Some("jellyfin") => "jellyfin",
        Some("subsonic") | Some("navidrome") | Some("gonic") | Some("airsonic")
        | Some("astiga") => "subsonic",
        _ => "local",
    };
    let is_offline = src == "qobuz_download";
    let is_plex = src == "plex";
    let is_media = src == "jellyfin" || src == "subsonic";
    let artwork_url =
        crate::local_rows::portable_artwork_ref(t, crate::local_rows::ArtworkScope::Track);
    let sample_rate_khz = if t.sample_rate >= 1000.0 {
        t.sample_rate / 1000.0
    } else {
        t.sample_rate
    };
    QueueTrack {
        id: if is_offline {
            t.qobuz_track_id.unwrap_or(t.id) as u64
        } else {
            t.id as u64
        },
        title: t.title.clone(),
        version: None,
        artist: t.artist.clone(),
        album: t.album_group_title.clone(),
        album_version: None,
        duration_secs: t.duration_secs,
        artwork_url,
        hires: t.bit_depth.map(|d| d > 16).unwrap_or(false),
        bit_depth: t.bit_depth,
        sample_rate: Some(sample_rate_khz),
        is_local: true,
        // Navigation key. For Plex the track's `album_group_key` is the
        // per-edition split key, which the album cache is NOT keyed by —
        // recover the content-hash key so "go to album" resolves.
        album_id: Some(if is_plex {
            crate::local_plex::album_key_for(&t.artist, &t.album)
        } else {
            // For a media row this is already the PREFIXED key
            // (`jellyfin:<albumId>`) that `cached_to_local_track` stamped, so
            // "go to album" from the queue opens the same page the card does.
            t.album_group_key.clone()
        }),
        artist_id: None,
        // GENUINELY always true, and this is not a stub (contract §5.3 F5).
        // This row is a FILE ON DISK — a local scan, a Plex item, or a
        // `qobuz_download` — and Qobuz's streaming rights do not reach it.
        // `qbz-core` takes the offline tier BEFORE it asks any availability
        // question (core.rs:716), which is exactly why a track Qobuz PULLED
        // still plays from here. Marking these `false` would delete the user's
        // own downloads from the queue — a worse bug than the one D5 fixes, and
        // it would bite hardest in offline mode, where this is the only copy
        // that exists.
        streamable: true,
        source: Some(src.to_string()),
        parental_warning: false,
        // Plex: the string rating_key the resolve needs (the numeric queue id
        // is a namespaced form). Offline: the local row id.
        source_item_id_hint: if is_plex || is_offline || is_media {
            Some(if is_plex || is_media {
                // Plex: the raw rating key. Media servers: the SERVER's own
                // item id, which `cached_to_local_track` parks in `file_path`
                // (a remote track has no path on this machine). `claim` prefers
                // it over the numeric id because it survives a cache rebuild.
                t.file_path.clone()
            } else {
                t.id.to_string()
            })
        } else {
            None
        },
        // NO container origin. `"local"` is a kind the Slint never emits and
        // nothing consumes: `playback.rs`'s own `local_queue_track`
        // (:1478-1481) leaves both fields None and lets the per-track ALBUM
        // fallback in `refresh_now_playing_meta` (:1959-1965) land the glyph on
        // the LocalAlbum view. Leaving it stamped also defeated the shared
        // seam — `stamp_context`'s already-stamped test is satisfied by
        // ("local", key), so routing these queues through `set_queue_stamped`
        // would have preserved the invented kind instead of deriving a real
        // one. Post-change: a single-album local queue derives ("album",
        // group_key); a folder / Tracks-tab queue spanning albums derives
        // nothing and falls back per track to the same key.
        context_kind: None,
        context_id: None,
        // Identity straight from the scanned tags (None on untagged files).
        isrc: t.isrc.clone(),
        recording_mbid: t.musicbrainz_recording_id.clone(),
    }
}

// ---------------------------------------------------------------------------
// Audible steps
// ---------------------------------------------------------------------------

/// Does this queue row belong to `qbz-core`'s OFFLINE tier rather than to the
/// local audible step?
///
/// A pure test on the row, deliberately: the alternative is asking the
/// filesystem whether `file_path` is a directory, and that stats a path which
/// may live on an unreachable share — the exact block the audible step's
/// bounded probe exists to survive.
pub(crate) fn belongs_to_the_offline_tier(track: &QueueTrack) -> bool {
    qbz_source::RawRef::from_queue_track(track).badge == qbz_source::SourceBadge::Offline
}

/// Route ONE queue track to its audible step — through `qbz-source`.
///
/// This used to `match track.source.as_deref()`, with a `Some("plex")` arm
/// carrying its own rating-key ladder (`plex_rating_key`, deleted with it) and
/// a `_` arm that handed `track.id` to `play_local_file`. Both were wrong in a
/// way only the seam can see:
///
/// - the `_` arm caught **offline** rows, whose `QueueTrack.id` is the *Qobuz*
///   catalog id while the `library.db` row id rides in `source_item_id_hint`
///   (`local_queue_track` above, `:161-167` and `:199-206`). It looked the row
///   up by the wrong number and missed. `LocalSource::row_id` decides that on
///   evidence instead;
/// - the `plex:`-prefixed-hint rule was one engineer's reconstruction of a
///   five-id table nobody had written down, and its fallback is right for the
///   MyQBZ path and wrong for the LocalLibrary one. `PlexSource` owns that
///   table now, and a shape it recognises but rejects is a named error at the
///   moment of the mistake rather than a 404 two layers down.
///
/// `SourceRegistry::playback` claims the row ONCE and answers with a ticket;
/// `audible_qt` performs it. No source is branched on by hand here any more.
///
/// The ONE routing decision left is not a source branch, it is a TIER one: an
/// OFFLINE row is a file the local step cannot read. `library.db` stores a
/// CMAF download's `file_path` as the BUNDLE DIRECTORY
/// (`…/audio/tracks-cmaf/<qobuz_id>/`, holding `init.mp4` + an encrypted
/// `segments.bin`), so `LocalSource::playback` hands back a
/// `PlaybackTicket::File` naming a directory and the read fails as
///
///   audible: file not available at …/audio/tracks-cmaf/266725026 (drive unmounted?)
///
/// — a message about a drive, for a bundle that is right there on disk. Only
/// `qbz-core`'s offline tier can decrypt it, and the row carries the real
/// Qobuz catalog id precisely so that tier can be asked. Two of the three
/// funnels into the audible step already excluded these rows
/// (`play_current_if_local`, `playback_qt::play_resolved_offline_aware`); the
/// LocalLibrary PLAY funnel did not, so every offline album played from that
/// view went silent. This is now the only copy of the test, and both local
/// play paths (here and `local_album_actions`) go through it.
pub(crate) async fn play_audible(runtime: &Runtime, track: &QueueTrack) -> bool {
    // A connected renderer owns playback — the same gate every Qobuz entry
    // passes (`route_play_remote`). Cheap when nothing is connected; without
    // it a Local Library row played beside a DLNA renderer.
    if crate::playback_qt::route_play_remote_track(runtime, track, "play_audible").await {
        return true;
    }
    let Some(_owner_action) = crate::playback_qt::begin_owner_action() else {
        // The delegated queue is already audible through its stamped engine;
        // a local click is a handled refusal, never a second backend start.
        return true;
    };
    if belongs_to_the_offline_tier(track) {
        return match crate::playback_qt::play_resolved_offline_aware(runtime, track.id, 0).await {
            Ok(()) => true,
            Err(e) => {
                log::error!(
                    "[qbz-qt] local play: offline track {} not playable: {e}",
                    track.id
                );
                false
            }
        };
    }
    match crate::audible_qt::play_queue_track(runtime, track).await {
        Ok(played) => played,
        Err(e) => {
            log::error!("[qbz-qt] local play: track {} not playable: {e}", track.id);
            false
        }
    }
}

fn report_staged_playback_failure(track: &QueueTrack) {
    let source = track.source.as_deref().unwrap_or("local");
    let message = if is_probeable_local_queue_row(track) {
        qbz_i18n::t(LOCAL_FILE_UNAVAILABLE)
    } else {
        format!(
            "{} playback failed; the previous track is still playing",
            source
        )
    };
    crate::local_bridge::emit_track_availability(source, track.id, Some(&message));
    crate::toast_qt::warning(message);
}

fn clear_staged_playback_failure(track: &QueueTrack) {
    crate::local_bridge::emit_track_availability(
        track.source.as_deref().unwrap_or("local"),
        track.id,
        None,
    );
}

/// What the local/Plex audible step did with a queue row.
///
/// This used to be a `bool`, and the two `false`s meant OPPOSITE things: "this
/// row is not mine, go run the Qobuz walk" and "this row IS mine and it cannot
/// play". The caller could only treat both as the first, so a local file on a
/// share that had gone away fell through to the Qobuz tier-walk, failed there
/// with an unrelated error ("No Qobuz client available"), and
/// `auto_skip_unavailable` then declined to skip because that error is not
/// terminal-unavailable. Net effect: one unreachable local file stopped
/// playback dead, silently — the exact shape of "a track that cannot play must
/// never be blocking".
pub enum LocalPlay {
    /// Not a local/Plex row. The Qobuz path must run, unchanged.
    NotLocal,
    /// Playing.
    Played,
    /// Ours, and it cannot play. Carries a reason phrased so the shared skip
    /// walk recognises it as terminal (see `is_terminal_unavailable`).
    Unavailable(String),
}

/// Source-aware AUDIBLE step for the shared poll-loop advance.
///
/// Without this seam, auto-advance to the next local/Plex track goes down
/// `play_track_resolved` and fails with "No Qobuz client available".
pub async fn play_current_if_local(runtime: &Runtime, track_id: u64) -> LocalPlay {
    let Some(qt) = runtime.core().current_track().await else {
        return LocalPlay::NotLocal;
    };
    if qt.id != track_id {
        return LocalPlay::NotLocal;
    }
    // WHO OWNS THIS ROW is asked ONCE, of the registry (design 02 §3.1's
    // vocabulary table), instead of matched against two hand-written words.
    //
    // The old test was `Some("local") | Some("plex")`, and it is IC-12: six
    // source words really occur on a queue row, and the four it missed —
    // `"user"`, `"ephemeral"`, `"qobuz_download"`, `"qobuz_purchase"` — all
    // name rows that are FILES ON DISK. Auto-advance onto one of them returned
    // `NotLocal`, fell through to the Qobuz tier-walk and died there with "No
    // Qobuz client available": an error about a service that has nothing to do
    // with the track, which `auto_skip_unavailable` then declines to skip
    // because it is not terminal-unavailable. One ephemeral track could stop
    // playback dead.
    //
    // `claim` refuses to guess, so a row nobody owns stays `NotLocal` and the
    // Qobuz path runs exactly as before.
    let raw = qbz_source::RawRef::from_queue_track(&qt);
    // An OFFLINE row is NOT ours, and this exclusion is the correction to
    // IC-12's first pass. `SourceId::from_word` folds `qobuz_download` /
    // `qobuz_purchase` / `offline` into LOCAL because the row IS a file — but
    // for a CMAF-cached download that "file" is the offline cache's own
    // container, and only `qbz-core`'s offline tier can read it. Handing it to
    // the local audible step produced exactly one log line and silence:
    //
    //   audible: file not available at …/audio/tracks-cmaf/426056576
    //
    // Its `QueueTrack.id` is the real Qobuz catalog id precisely so the funnel
    // can resolve it, which is what happened before stage 3 and what has to
    // keep happening.
    let ours = match qbz_source::registry().claim(&raw) {
        Ok(item) => {
            item.source() != qbz_source::SourceId::QOBUZ && !belongs_to_the_offline_tier(&qt)
        }
        Err(_) => false,
    };
    if !ours {
        return LocalPlay::NotLocal;
    }
    if play_audible(runtime, &qt).await {
        LocalPlay::Played
    } else {
        // The wording is load-bearing: `is_terminal_unavailable` matches on
        // "no longer available", which is what routes this into the SAME
        // bounded skip walk a pulled Qobuz track takes. One seam for both,
        // which is the point — to the user they are the same failure.
        LocalPlay::Unavailable(format!("local file no longer available (track {track_id})"))
    }
}

// ---------------------------------------------------------------------------
// Queue builders
// ---------------------------------------------------------------------------

/// Set the queue to `tracks` and start `start` (playback.rs
/// `play_local_tracks_now`: `set_queue` then the AUDIBLE step).
pub(crate) async fn play_rows(
    runtime: &Runtime,
    tracks: Vec<LocalTrack>,
    start: usize,
    shuffle: bool,
) {
    if tracks.is_empty() {
        return;
    }
    // Establish the actual first row before doing any filesystem artwork work
    // or mutating shuffle/queue state. A shuffle click must validate the row it
    // will really start, not the old ordered anchor.
    let mut tracks = tracks;
    let start = if shuffle {
        crate::playback_qt::xorshift_shuffle(&mut tracks);
        0
    } else {
        start.min(tracks.len() - 1)
    };
    let candidate = local_queue_track(&tracks[start]);
    if preflight_queue_track(&candidate).await.is_err() {
        return;
    }
    // Folder-cover backfill BEFORE the mapping (the reference runs it at every
    // local play site: playback.rs:1094 / :1322 / :1349 / :1382). This is the
    // funnel for album / folder / folder-track / Tracks-tab play, so one call
    // here covers all four. Blocking fs, hence spawn_blocking.
    let tracks = tokio::task::spawn_blocking(move || {
        let mut tracks = tracks;
        fill_missing_covers(&mut tracks);
        tracks
    })
    .await
    .unwrap_or_default();
    if tracks.is_empty() {
        return;
    }
    let queue: Vec<QueueTrack> = tracks.iter().map(local_queue_track).collect();
    // Shuffle reorders THIS list and starts at the top of the mixed order. The
    // mode alone only randomises what comes next, so the first track the user
    // hears was the folder's #1 every time — owner ruling 2026-08-01, every
    // shuffle must be genuinely random. The caller's anchor is meaningless once
    // the order is mixed, so it is dropped (`play_track_list_in` does the same).
    let start = if shuffle { 0 } else { start };
    // Through the SHARED seam (not `core().set_queue`) so the origin is derived
    // from the queue: one album -> ("album", group_key), a mixed folder queue ->
    // nothing, and the per-track album fallback lands it on the same view.
    // Local rows are never dropped by the seam — `local_queue_track` sets
    // `streamable: true` (a file on disk is outside Qobuz's rights) and
    // `is_track_blacklisted` fails open for any source that is not "qobuz" — so
    // the anchor always comes back and always names `first`. Answered anyway
    // rather than discarded with a `let _`: the day a Qobuz row reaches this
    // path, this returns instead of trying to play a track the core dropped.
    let Some(prepared) = crate::playback_qt::prepare_queue_stamped(queue, Some(start), None) else {
        log::warn!("[qbz-qt] local play: the queue was filtered to nothing");
        return;
    };
    let Some(first) = prepared.anchor_track().cloned() else {
        log::warn!("[qbz-qt] local play: prepared queue has no anchor");
        return;
    };
    let Some(_owner_action) = crate::playback_qt::begin_owner_action() else {
        return;
    };

    // A source-owned play is staged: first prove that the player accepted the
    // replacement, then publish its queue. A failed Plex part or moved local
    // file therefore leaves the currently audible track, Queue, NPB and MPRIS
    // in agreement. Offline Qobuz bundles are the one exception because their
    // resolver reads the current core row to find its encrypted container.
    let offline = belongs_to_the_offline_tier(&first);
    if !offline && !play_audible(runtime, &first).await {
        report_staged_playback_failure(&first);
        return;
    }
    crate::playback_qt::commit_prepared_queue(runtime, prepared).await;
    if shuffle {
        runtime.core().set_shuffle(true).await;
        crate::now_playing::set_shuffle(true);
    }
    crate::playback_qt::publish_queue(runtime).await;
    if offline && !play_audible(runtime, &first).await {
        report_staged_playback_failure(&first);
        return;
    }
    clear_staged_playback_failure(&first);
    crate::playback_qt::refresh_now_playing(runtime).await;
}

/// Play a whole album (its group key, local or `plex:<hash>`) from the top,
/// or from `start_track_id` when a row was clicked.
pub async fn play_album(
    runtime: &Runtime,
    album_id: String,
    start_track_id: Option<i64>,
    shuffle: bool,
) {
    let key = album_id.clone();
    let tracks = tokio::task::spawn_blocking(move || fetch_album_tracks_blocking(&key))
        .await
        .unwrap_or_default();
    let start = start_track_id
        .and_then(|tid| tracks.iter().position(|t| t.id == tid))
        .unwrap_or(0);
    play_rows(runtime, tracks, start, shuffle).await;
}

/// Play only the physical copies admitted by the active Local Library media
/// funnel. A logical album card can represent several servers and a local
/// directory; reopening the unfiltered group here would silently bypass the
/// source/format/quality choices the user is looking at.
pub async fn play_album_filtered(
    runtime: &Runtime,
    album_id: String,
    filter_json: String,
    shuffle: bool,
) {
    let tracks = tokio::task::spawn_blocking(move || {
        let filter = crate::local_filter::MediaFilter::from_json(&filter_json);
        fetch_album_tracks_blocking(&album_id)
            .into_iter()
            .filter(|track| filter.track_enabled(track))
            .collect()
    })
    .await
    .unwrap_or_default();
    play_rows(runtime, tracks, 0, shuffle).await;
}

/// Play everything under a folder, recursively, in path order.
pub async fn play_folder(runtime: &Runtime, path: String) {
    let tracks = tokio::task::spawn_blocking(move || {
        with_db(|db| db.list_folder_tracks_recursive(&path, false)).unwrap_or_default()
    })
    .await
    .unwrap_or_default();
    play_rows(runtime, tracks, 0, false).await;
}

/// Play a folder's DIRECT tracks starting at the clicked row.
pub async fn play_folder_track(runtime: &Runtime, folder: String, row_id: i64) {
    let tracks = tokio::task::spawn_blocking(move || {
        with_db(|db| db.list_folder_tracks(&folder, false)).unwrap_or_default()
    })
    .await
    .unwrap_or_default();
    let start = tracks.iter().position(|t| t.id == row_id).unwrap_or(0);
    play_rows(runtime, tracks, start, false).await;
}

/// Reorder the loaded raw rows into the order the Tracks tab is RENDERING and
/// locate the clicked row inside it.
///
/// `None` — and the caller falls back to the SQL-page order — when the clicked
/// id does not resolve inside the ordered list. Same guard, same reason as
/// `library_qt::order_by_visible` (the port of `playback.rs::order_by_visible`,
/// :3408-3428): better the old queue than a queue that starts on the wrong row.
///
/// Ids arrive as strings because that is what the QML rows carry
/// (`local_rows::TrackRow.id`); unparseable ones are dropped.
pub(crate) fn order_by_visible(
    rows: &[LocalTrack],
    visible_ids: &[String],
    clicked_id: i64,
) -> Option<(Vec<LocalTrack>, usize)> {
    let by_id: std::collections::HashMap<i64, &LocalTrack> =
        rows.iter().map(|t| (t.id, t)).collect();
    let ordered: Vec<LocalTrack> = visible_ids
        .iter()
        .filter_map(|id| id.parse::<i64>().ok())
        .filter_map(|id| by_id.get(&id).map(|t| (*t).clone()))
        .collect();
    let idx = ordered.iter().position(|t| t.id == clicked_id)?;
    Some((ordered, idx))
}

/// Tracks tab row click (PARITY-DEBT #14): the ALREADY-LOADED page set becomes
/// the queue so playback continues down the list, in the order ON SCREEN. No
/// DB re-query — the raw rows are kept by the loader, which is also what makes
/// a merged PLEX row playable (it has no `local_tracks` row to re-query).
///
/// `visible_ids_json` is the JSON string array of the track ids the tab is
/// CURRENTLY rendering, in render order; `clicked_id` is the row that was hit.
///
/// The Tracks tab's SORT is server-side (it defines the pagination order), but
/// its GROUP modes ("by album" / "by artist" / "by name") are a client-side
/// visual reorder on top of the loaded pages — the reference is explicit that
/// they "keep their client-side visual reorder on top" (commit `e379aa65`).
/// Queueing `tracks_raw` therefore played the SQL order while the user was
/// looking at the grouped one: click row 3 of the "Air" group and you heard
/// whatever happened to sit at SQL offset 3. The order can only come from the
/// view (it is derived in QML, like `LibraryView`'s), so it comes down as an
/// id array and the raw rows play the part of the authoritative cache.
pub async fn play_tracks_visible(runtime: &Runtime, visible_ids_json: String, clicked_id: i64) {
    let rows: Vec<LocalTrack> = state(|s| s.tracks_raw.clone());
    if rows.is_empty() {
        return;
    }
    let ids: Vec<String> = serde_json::from_str(&visible_ids_json).unwrap_or_default();
    match order_by_visible(&rows, &ids, clicked_id) {
        Some((ordered, start)) => play_rows(runtime, ordered, start, false).await,
        None => {
            let start = rows.iter().position(|t| t.id == clicked_id).unwrap_or(0);
            play_rows(runtime, rows, start, false).await;
        }
    }
}

/// Look up ONE raw row by id: the loaded Tracks page first, then the open
/// detail pane, then `library.db` (Plex ids never reach the DB — they are
/// namespaced and only ever live in the cached documents).
pub(crate) fn find_track_blocking(row_id: i64) -> Option<LocalTrack> {
    let cached = state(|s| {
        s.tracks_raw
            .iter()
            .chain(s.detail_raw.iter())
            .chain(s.genre_detail_raw.values().flatten())
            .find(|t| t.id == row_id)
            .cloned()
    });
    if cached.is_some() {
        return cached;
    }
    if crate::local_plex::is_plex_track_id(row_id) {
        return None;
    }
    with_db(|db| db.get_track(row_id)).flatten()
}

/// Row / card "Play next" / "Play later" / "Add to queue".
/// `kind` = "track" | "album" | "folder"; `mode` = "next" | "later" | queue.
pub async fn enqueue(runtime: &Runtime, kind: String, id: String, mode: String) {
    let k = kind.clone();
    let ident = id.clone();
    let tracks: Vec<LocalTrack> = tokio::task::spawn_blocking(move || {
        let mut rows: Vec<LocalTrack> = match k.as_str() {
            "track" => ident
                .parse::<i64>()
                .ok()
                .and_then(find_track_blocking)
                .into_iter()
                .collect(),
            "folder" => {
                with_db(|db| db.list_folder_tracks_recursive(&ident, false)).unwrap_or_default()
            }
            _ => fetch_album_tracks_blocking(&ident),
        };
        // Same backfill the play funnel does — an enqueued row must carry the
        // same cover it would have carried had it been played
        // (reference: main.rs:10636 fills before `enqueue_local_tracks`).
        fill_missing_covers(&mut rows);
        rows
    })
    .await
    .unwrap_or_default();
    enqueue_rows(runtime, tracks, mode).await;
}

/// Enqueue only the physical album copies admitted by the active media
/// funnel. This is the card/context-menu counterpart of
/// `play_album_filtered`.
pub async fn enqueue_album_filtered(
    runtime: &Runtime,
    album_id: String,
    filter_json: String,
    mode: String,
) {
    let tracks = tokio::task::spawn_blocking(move || {
        let filter = crate::local_filter::MediaFilter::from_json(&filter_json);
        let mut rows: Vec<LocalTrack> = fetch_album_tracks_blocking(&album_id)
            .into_iter()
            .filter(|track| filter.track_enabled(track))
            .collect();
        fill_missing_covers(&mut rows);
        rows
    })
    .await
    .unwrap_or_default();
    enqueue_rows(runtime, tracks, mode).await;
}

pub(crate) async fn enqueue_rows(runtime: &Runtime, tracks: Vec<LocalTrack>, mode: String) {
    if tracks.is_empty() {
        return;
    }
    // Same stamping seam the Qobuz enqueue paths use, so an appended local
    // block carries its own origin instead of inheriting whatever is playing.
    let queue = crate::playback_qt::stamped(tracks.iter().map(local_queue_track).collect(), None);
    if queue.is_empty() {
        return;
    }
    let Some(_owner_action) = crate::playback_qt::begin_owner_action() else {
        return;
    };
    // Same core helpers the Qobuz rows use: "next" inserts at the cursor
    // (reversed so a multi-track insert keeps its order), "later" appends to
    // the block tail, anything else appends.
    //
    // This remains a local-only execution path. While QConnect is enabled the
    // shared seam above returns an empty batch after raising the counted
    // notice, so no local row can enter the connected queue here.
    match mode.as_str() {
        "next" => {
            for t in queue.into_iter().rev() {
                runtime.core().add_track_next(t).await;
            }
        }
        "later" => {
            for t in queue {
                runtime.core().add_track_later(t).await;
            }
        }
        _ => runtime.core().add_tracks(queue).await,
    }
    crate::playback_qt::publish_queue(runtime).await;
}

#[cfg(test)]
mod tests {
    use super::order_by_visible;
    use qbz_library::LocalTrack;

    fn track(id: i64, title: &str) -> LocalTrack {
        LocalTrack {
            id,
            title: title.to_string(),
            ..Default::default()
        }
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn titles(rows: &[LocalTrack]) -> Vec<&str> {
        rows.iter().map(|t| t.title.as_str()).collect()
    }

    /// PARITY-DEBT #14: the queue follows the VISIBLE order (here: reversed by
    /// a group mode), not the SQL page order the loader cached.
    #[test]
    fn queue_follows_the_visible_order() {
        let rows = vec![track(1, "a"), track(2, "b"), track(3, "c")];
        let (ordered, start) =
            order_by_visible(&rows, &ids(&["3", "1", "2"]), 1).expect("clicked row resolves");
        assert_eq!(titles(&ordered), vec!["c", "a", "b"]);
        assert_eq!(start, 1);
    }

    /// Ids the loaded page does not have (a row scrolled out of the raw cache,
    /// a stale report) are dropped rather than poisoning the queue, and the
    /// start index still points at the clicked row AFTER the drop.
    #[test]
    fn unknown_and_unparseable_ids_are_dropped() {
        let rows = vec![track(1, "a"), track(2, "b")];
        let (ordered, start) = order_by_visible(&rows, &ids(&["99", "not-a-number", "2", "1"]), 1)
            .expect("clicked row resolves");
        assert_eq!(titles(&ordered), vec!["b", "a"]);
        assert_eq!(start, 1);
    }

    /// The clicked row not being in the visible list is the one case the
    /// caller must NOT start a queue from: `None` sends it to the SQL-order
    /// fallback instead of playing the wrong track.
    #[test]
    fn clicked_row_outside_the_visible_list_is_none() {
        let rows = vec![track(1, "a"), track(2, "b")];
        assert!(order_by_visible(&rows, &ids(&["2"]), 1).is_none());
        assert!(order_by_visible(&rows, &[], 1).is_none());
    }

    /// A DOWNLOADED row played from Local Library must be recognised as the
    /// offline tier's, not the local audible step's. The regression this
    /// guards was silent: `LocalSource` answered with a `File` ticket naming
    /// the CMAF bundle DIRECTORY, the read failed as "drive unmounted?", and
    /// the bar still adopted the track — the album looked like it was playing
    /// and no audio came out.
    ///
    /// The test runs the row through the REAL `local_queue_track`, because the
    /// two halves that have to agree are its source word and the badge the
    /// router reads. Asserting the badge alone would keep passing if either
    /// side were renamed.
    #[test]
    fn a_downloaded_row_is_the_offline_tiers_not_the_local_steps() {
        let offline = LocalTrack {
            id: 4938,
            qobuz_track_id: Some(266_725_026),
            source: Some("qobuz_download".into()),
            file_path: "/c/qbz/audio/tracks-cmaf/266725026".into(),
            ..Default::default()
        };
        let row = super::local_queue_track(&offline);
        assert!(super::belongs_to_the_offline_tier(&row));
        // And it is asked for by the QOBUZ id — the offline tier is keyed on
        // it, and the library row id rides in the hint.
        assert_eq!(row.id, 266_725_026);
        assert_eq!(row.source_item_id_hint.as_deref(), Some("4938"));
    }

    /// The other side of the same fork: a user file, a Plex item and a media
    /// server row all stay with the local audible step. A predicate that said
    /// "yes" here would send a file on disk down the Qobuz tier walk.
    #[test]
    fn every_other_local_source_stays_with_the_audible_step() {
        for word in ["user", "plex", "jellyfin", "subsonic", "navidrome"] {
            let t = LocalTrack {
                id: 12,
                source: Some(word.into()),
                file_path: "/m/a.flac".into(),
                ..Default::default()
            };
            let row = super::local_queue_track(&t);
            assert!(
                !super::belongs_to_the_offline_tier(&row),
                "{word} must not be routed to the offline tier"
            );
        }
    }

    /// The album card may correctly use the box cover, but every queue row
    /// must carry its own disc folder's cover. NPB, MPRIS and notifications
    /// all consume `QueueTrack.artwork_url`; if this mapping collapses, those
    /// three surfaces disagree with the disc divider and track hover.
    #[test]
    fn box_set_queue_rows_keep_each_discs_own_artwork() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let box_dir =
            std::env::temp_dir().join(format!("qbz-queue-disc-art-{}-{nonce}", std::process::id()));
        // `#` is the exact seam this fixture protects: QueueTrack carries a
        // file URI (`%23`), while the NPB/queue resolver must stat the decoded
        // filesystem path and MPRIS must not publish a double-escaped `%2523`.
        let disc_one = box_dir.join("Disc 01 - TV Series Soundtrack #01");
        let disc_five = box_dir.join("Disc 05 - Movie OST");
        std::fs::create_dir_all(&disc_one).unwrap();
        std::fs::create_dir_all(&disc_five).unwrap();
        std::fs::write(box_dir.join("cover.jpg"), b"box").unwrap();
        std::fs::write(disc_one.join("cover.jpg"), b"one").unwrap();
        std::fs::write(disc_five.join("cover.jpg"), b"five").unwrap();

        let box_cover = box_dir.join("cover.jpg").to_string_lossy().into_owned();
        let mut tracks = vec![
            LocalTrack {
                id: 1,
                source: Some("user".into()),
                file_path: disc_one.join("01.flac").to_string_lossy().into_owned(),
                artwork_path: Some(box_cover.clone()),
                ..Default::default()
            },
            LocalTrack {
                id: 5,
                source: Some("user".into()),
                file_path: disc_five.join("01.flac").to_string_lossy().into_owned(),
                artwork_path: Some(box_cover),
                ..Default::default()
            },
        ];

        super::fill_missing_covers(&mut tracks);
        let one = super::local_queue_track(&tracks[0]);
        let five = super::local_queue_track(&tracks[1]);
        assert_ne!(one.artwork_url, five.artwork_url);
        assert!(one.artwork_url.as_deref().unwrap().contains("Disc 01"));
        assert!(five.artwork_url.as_deref().unwrap().contains("Disc 05"));
        let one_resolved = crate::artwork_qt::cached_path(one.artwork_url.as_deref().unwrap());
        // Compare separator-normalised: a `file://` URL is always `/`-separated,
        // so the round trip through it cannot hand back a host's backslashes.
        // Both forms open the same file on Windows.
        let slash = |p: &str| p.replace('\\', "/");
        assert_eq!(
            slash(&crate::artwork_qt::local_path(&one_resolved)),
            slash(&disc_one.join("cover.jpg").to_string_lossy())
        );

        std::fs::remove_dir_all(&box_dir).unwrap();
    }

    /// Qobuz downloads are encrypted bundle directories, not ordinary album
    /// folders. Keep their historical on-disk fallback when the index has no
    /// art, but never replace an artwork URL already supplied by the catalog.
    #[test]
    fn qobuz_bundle_cover_is_fallback_only() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let bundle = std::env::temp_dir().join(format!(
            "qbz-qobuz-bundle-cover-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir_all(&bundle).unwrap();
        std::fs::write(bundle.join("cover.jpg"), b"bundle").unwrap();

        let row = |artwork_path| LocalTrack {
            source: Some("qobuz_download".into()),
            file_path: bundle.to_string_lossy().into_owned(),
            artwork_path,
            ..Default::default()
        };
        let mut tracks = vec![
            row(None),
            row(Some(String::new())),
            row(Some("https://catalog/art.jpg".into())),
        ];

        super::fill_missing_covers(&mut tracks);
        assert!(tracks[0]
            .artwork_path
            .as_deref()
            .unwrap()
            .ends_with("cover.jpg"));
        assert!(tracks[1]
            .artwork_path
            .as_deref()
            .unwrap()
            .ends_with("cover.jpg"));
        assert_eq!(
            tracks[2].artwork_path.as_deref(),
            Some("https://catalog/art.jpg")
        );

        std::fs::remove_dir_all(&bundle).unwrap();
    }
}

// ---------------------------------------------------------------------------
// Source-aware single-track play (Home's Recently-Played rail, 2026-08-13)
// ---------------------------------------------------------------------------

/// Play ONE non-Qobuz track by its queue id, resolving it through its own
/// source. Returns false when the row cannot be resolved.
///
/// WHY THIS EXISTS. `playback_qt::queue_track_for` has exactly two arms — the
/// Qobuz favourites feed, then `get_track` against the catalog — so any id that
/// misses the feed goes to the Qobuz API. For a local or Plex id that is a
/// guaranteed 404 (reported by the owner from Discover's Recently-Played rail:
/// `get_track(1099511673001)` on a `PLEX_TRACK_ID_FLOOR`-namespaced id). The
/// cortinilla has routed by source since it was written and says why in its own
/// comment: handing a local id to the catalog "would 404 or, worse, open
/// someone else's album that happens to share the number". This is that seam
/// for the rails that reach playback through the generic by-id entry.
///
/// ROUTING IS BY THE ROW'S `source`, NEVER BY ID ARITHMETIC. `id >=
/// PLEX_TRACK_ID_FLOOR` is only unambiguous for Plex; plain local ids are small
/// `library.db` rowids that CAN collide with real Qobuz track ids, so a
/// speculative `get_track()` on the local DB could play the wrong file. It is
/// also what keeps Jellyfin/Navidrome a match arm here instead of another sweep
/// through the tree.
///
/// An unresolvable row plays NOTHING and says so. Falling back to the Qobuz
/// path would reproduce the 404 this exists to remove, and falling back to
/// "some other track" is the failure the cortinilla explicitly refuses.
pub async fn play_single_from_source(runtime: &Runtime, track_id: u64, source: &str) -> bool {
    let track = match source {
        // A library row: the DB has everything, and `local_queue_track` builds
        // the QueueTrack the audible step expects.
        "local" | "qobuz_download" => tokio::task::spawn_blocking(move || {
            with_db(|db| db.get_track(track_id as i64)).flatten()
        })
        .await
        .ok()
        .flatten(),
        // A Plex row is SYNTHETIC — `local_plex::map_cached_to_local_track`
        // mints it and it never touches library.db, so there is no row to read.
        // The cache is queried by ALBUM, and the recently-played entry carries
        // the album artist/title, which is exactly the key. Resolving through
        // the real cached row is what recovers `rating_key` (the audible step
        // needs it as `source_item_id_hint`, and it is NOT derivable from the
        // namespaced id: the id packs the cache ROWID, the hint is a different
        // column).
        "plex" => {
            let entry = crate::recently_qt::find_track(&track_id.to_string());
            let Some(entry) = entry else {
                log::warn!("[qbz-qt] plex play: {track_id} is not in the recently-played store");
                return false;
            };
            let key = crate::local_plex::album_key_for(&entry.album_artist, &entry.album_title);
            tokio::task::spawn_blocking(move || {
                crate::local_plex::album_tracks(&key)
                    .into_iter()
                    .find(|t| t.id as u64 == track_id)
            })
            .await
            .ok()
            .flatten()
        }
        // The media servers resolve through the SEAM, not through a
        // `LocalTrack`: their rows live in the shared remote cache and have no
        // `library.db` row to mint one from. `registry.tracks_of` claims the
        // namespaced id and hands back a queue row directly, which is the
        // shape this function was building by hand for the other sources.
        "jellyfin" | "subsonic" | "navidrome" | "gonic" | "airsonic" | "astiga" => {
            let raw = qbz_source::RawRef {
                source: qbz_source::SourceId::from_word(source),
                kind: Some(qbz_source::ItemKind::Track),
                id: track_id.to_string(),
                is_local: Some(true),
                ..Default::default()
            };
            return match qbz_source::registry().tracks_of(&raw).await {
                Ok(tracks) if !tracks.is_empty() => {
                    let Some(prepared) =
                        crate::playback_qt::prepare_queue_stamped(tracks, Some(0), None)
                    else {
                        log::warn!("[qbz-qt] {source} play: the queue was filtered to nothing");
                        return false;
                    };
                    let Some(first) = prepared.anchor_track().cloned() else {
                        log::warn!("[qbz-qt] {source} play: prepared queue has no anchor");
                        return false;
                    };
                    let Some(_owner_action) = crate::playback_qt::begin_owner_action() else {
                        return true;
                    };
                    let played = play_audible(runtime, &first).await;
                    if !played {
                        report_staged_playback_failure(&first);
                        return false;
                    }
                    crate::playback_qt::commit_prepared_queue(runtime, prepared).await;
                    crate::playback_qt::publish_queue(runtime).await;
                    clear_staged_playback_failure(&first);
                    crate::playback_qt::refresh_now_playing(runtime).await;
                    played
                }
                Ok(_) => {
                    log::warn!("[qbz-qt] {source} play: track {track_id} resolved to nothing");
                    false
                }
                Err(e) => {
                    log::warn!("[qbz-qt] {source} play: track {track_id} not resolvable: {e}");
                    false
                }
            };
        }
        other => {
            log::warn!("[qbz-qt] play: unknown source {other:?} for track {track_id}");
            return false;
        }
    };
    let Some(track) = track else {
        log::warn!("[qbz-qt] play: {source} track {track_id} could not be resolved");
        return false;
    };
    play_rows(runtime, vec![track], 0, false).await;
    true
}
