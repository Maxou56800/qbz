//! Album Info (Credits / Review) modal document — the Qt port of
//! `qbz/src/info_modals.rs::open_album_credits` + `map_album_credits`,
//! opened by the AlbumView header's info button. Layout reference per the
//! project ruling for this feature: Tauri's `AlbumCreditsModal.svelte`
//! (left fixed meta column, right scrollable Credits/Review tabs).
//!
//! Performer parsing stays in the shared crate
//! (`qbz_qobuz::performers`, ADR-006); this module fetches the album and
//! shapes the JSON document `qml/shell/AlbumInfoModal.qml` parses. Tab
//! switching and close are QML-local state — the doc carries data only.

use std::sync::Arc;

use qbz_app::shell::AppRuntime;
use qbz_core::LoggingAdapter;
use qbz_models::{Album, Track};
use qbz_qobuz::performers::parse_performers;
use serde::Serialize;

use crate::album_bridge;
use cxx_qt_lib::QString;

#[derive(Serialize)]
struct PerformerRow {
    name: String,
    /// ", Role1, Role2" suffix (empty when role-less) — RAW roles, not
    /// localized, 1:1 with Tauri (Slint `info_modals.rs::roles_suffix`).
    roles: String,
    /// First role or "Performer" — the musician-nav hint
    /// (Tauri `handlePerformerClick`: roles[0] || 'Performer').
    #[serde(rename = "primaryRole")]
    primary_role: String,
}

#[derive(Serialize)]
struct InfoTrack {
    id: String,
    number: String,
    title: String,
    artist: String,
    #[serde(rename = "hasCredits")]
    has_credits: bool,
    performers: Vec<PerformerRow>,
    copyright: String,
}

#[derive(Serialize, Default)]
struct AlbumInfoDoc {
    error: String,
    /// The album id, so the modal's per-track play can rebuild the queue
    /// context (QbzPlayer.playAlbumFrom).
    #[serde(rename = "albumId")]
    album_id: String,
    title: String,
    artist: String,
    /// Best-quality artwork URL; the modal falls back to the custom-cover
    /// override path when one is set (same rule as the header).
    #[serde(rename = "artUrl")]
    art_url: String,
    #[serde(rename = "customCoverPath")]
    custom_cover_path: String,
    label: String,
    #[serde(rename = "labelId")]
    label_id: String,
    /// Full localized date ("September 2, 2021").
    #[serde(rename = "releaseDate")]
    release_date: String,
    /// "Hard Rock · 10 tracks · 1h 21m" — gated on a genre, 1:1 with Tauri.
    #[serde(rename = "metaLine")]
    meta_line: String,
    /// "24-Bit / 96 kHz" (Tauri `formatQuality`; empty when unknown).
    quality: String,
    review: String,
    #[serde(rename = "hasReview")]
    has_review: bool,
    tracks: Vec<InfoTrack>,
}

/// "1h 21m" / "45m" (Slint `album_duration`; Tauri `formatAlbumDuration`).
fn album_duration(secs: u32) -> String {
    let hours = secs / 3600;
    let minutes = (secs % 3600) / 60;
    if hours > 0 {
        qbz_i18n::t_args("{} h {} min", &[&hours.to_string(), &minutes.to_string()])
    } else {
        qbz_i18n::t_args("{} min", &[&minutes.to_string()])
    }
}

/// Sample rate without a trailing ".0" (96.0 -> "96", 44.1 -> "44.1") — JS
/// number interpolation parity, NOT normalized to kHz from Hz.
fn fmt_rate(rate: f64) -> String {
    if rate.fract().abs() < f64::EPSILON {
        format!("{}", rate as i64)
    } else {
        format!("{rate}")
    }
}

/// "24-Bit / 96 kHz" (capital Bit, space before kHz) — Tauri
/// `formatQuality(bitDepth, samplingRate)`; empty when both are absent.
fn album_quality(bit: Option<u32>, rate: Option<f64>) -> String {
    match (bit, rate) {
        (Some(b), Some(r)) => format!("{b}-Bit / {} kHz", fmt_rate(r)),
        (Some(b), None) => format!("{b}-Bit"),
        (None, Some(r)) => format!("{} kHz", fmt_rate(r)),
        (None, None) => String::new(),
    }
}

/// ", Role1, Role2" or "" (raw roles, not localized — Tauri parity).
fn roles_suffix(roles: &[String]) -> String {
    if roles.is_empty() {
        String::new()
    } else {
        format!(", {}", roles.join(", "))
    }
}

fn map_track(index: usize, t: &Track, album_artist: &str) -> InfoTrack {
    let performers: Vec<PerformerRow> =
        parse_performers(t.performers.as_deref().unwrap_or_default())
            .into_iter()
            // Catalog fillers ("Not Documented") are not people (2026-09-13).
            .filter(|p| !qbz_text_utils::names::is_placeholder_name(&p.name))
            .map(|p| PerformerRow {
                roles: roles_suffix(&p.roles),
                primary_role: p
                    .roles
                    .first()
                    .cloned()
                    .unwrap_or_else(|| qbz_i18n::t("Performer")),
                name: p.name,
            })
            .collect();
    let copyright = t
        .copyright
        .as_deref()
        .map(qbz_text_utils::strip_html::decode_html_entities)
        .unwrap_or_default();
    let number = if t.track_number > 0 {
        t.track_number.to_string()
    } else {
        (index + 1).to_string()
    };
    let artist = t
        .performer
        .as_ref()
        .map(|a| a.name.clone())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| {
            if album_artist.is_empty() {
                qbz_i18n::t("Unknown Artist")
            } else {
                album_artist.to_string()
            }
        });
    InfoTrack {
        id: t.id.to_string(),
        number,
        title: crate::album_qt::format_album_title(&t.title, t.version.as_deref()),
        artist,
        has_credits: !performers.is_empty() || !copyright.is_empty(),
        performers,
        copyright,
    }
}

/// Is this a Qobuz CATALOG album id?
///
/// Qobuz album ids are opaque ASCII-alphanumeric strings and only a minority
/// of them are digits-only: of the 139 ids in the captured fixtures
/// (`qbz-nix-docs/qobuz-api/`), **114 are alphanumeric** (`vy32kidwrer67`) and
/// 25 are barcode-shaped (`0886446573373`). A local/Plex key is never that:
/// it is a filesystem path or a `plex:<rating key>`.
fn is_catalog_album_id(id: &str) -> bool {
    !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric())
}

fn map(album: Album, requested_id: &str) -> AlbumInfoDoc {
    let album_artist = album.artist.name.clone();
    let (label, label_id) = match album.label.as_ref() {
        Some(l) => (l.name.clone(), l.id.to_string()),
        None => (String::new(), String::new()),
    };
    let raw_tracks = album
        .tracks
        .as_ref()
        .map(|c| c.items.clone())
        .unwrap_or_default();
    let track_count = album
        .tracks_count
        .or(album.track_count)
        .unwrap_or(raw_tracks.len() as u32);

    // Tauri always appends formatAlbumDuration(album.duration || 0), so an
    // absent duration shows "· 0m" rather than dropping it.
    let meta_line = match album.genre.as_ref().filter(|g| !g.name.is_empty()) {
        Some(g) => [
            g.name.clone(),
            qbz_i18n::tf(
                "{} track",
                "{} tracks",
                track_count as i64,
                &[&track_count.to_string()],
            ),
            album_duration(album.duration.unwrap_or(0)),
        ]
        .join(" · "),
        None => String::new(),
    };

    let review = album
        .description
        .as_deref()
        .map(qbz_text_utils::strip_html::strip_html)
        .unwrap_or_default();
    let has_review = !review.trim().is_empty();

    // The REQUESTED id, not `album.id`. The QML holds its data overlay until
    // the published document answers for the album the user asked about, so an
    // id that came back in any other shape would leave the card blank forever
    // with nothing in the log — the exact-string-equality trap. The requested
    // id is also the right playback context: it is the one AlbumView itself
    // uses for `playAlbumFrom`.
    if album.id != requested_id {
        log::warn!(
            "[qbz-qt] album info: /album/get returned id '{}' for requested '{requested_id}'",
            album.id
        );
    }

    AlbumInfoDoc {
        error: String::new(),
        album_id: requested_id.to_string(),
        title: crate::album_qt::format_album_title(&album.title, album.version.as_deref()),
        artist: album_artist.clone(),
        art_url: album.image.best().cloned().unwrap_or_default(),
        custom_cover_path: crate::cover_artwork_qt::album_cover(&album.id).unwrap_or_default(),
        label,
        label_id,
        release_date: qbz_text_utils::dates::full_release_label(
            album.release_date_original.as_deref(),
        ),
        meta_line,
        quality: album_quality(album.maximum_bit_depth, album.maximum_sampling_rate),
        review,
        has_review,
        tracks: raw_tracks
            .iter()
            .enumerate()
            .map(|(i, t)| map_track(i, t, &album_artist))
            .collect(),
    }
}

fn publish(doc: &AlbumInfoDoc, loading: bool) {
    let json = serde_json::to_string(doc).unwrap_or_else(|_| "{}".into());
    album_bridge::ui(move |mut b| {
        b.as_mut().set_album_info_json(QString::from(json.as_str()));
        b.as_mut().set_album_info_loading(loading);
    });
}

/// Header info button: open the modal in its loading state, fetch the album
/// fresh (the credits/copyright fields are not in the view's document), then
/// publish. Non-catalog ids (local/Plex keys) are rejected — the button is
/// hidden for those, this is the backstop.
///
/// The generation guard is load-bearing: open A → close → open B leaves A's
/// fetch in flight, and without the guard a slow A publishes ITS document
/// over B's loading state — the modal then shows album A's credits inside
/// album B's page (seen in smoke 2026-08-10).
///
/// The id guard used to be `album_id.parse::<u64>()`, which had the predicate
/// backwards: it rejected the 4-in-5 Qobuz albums whose id is alphanumeric
/// while admitting local row ids. A rejected open returns before touching the
/// bridge, so the modal kept whatever document was there — the previous
/// album's, or, once the QML started holding its overlay until the ids match,
/// an empty card. Neither said a word above debug level. That is the whole of
/// "only the first album info of a session opens".
pub fn open(album_id: String) {
    if !is_catalog_album_id(&album_id) {
        log::warn!("[qbz-qt] album info skipped for non-catalog id '{album_id}'");
        return;
    }
    let generation = INFO_GENERATION.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
    album_bridge::ui(|mut b| {
        b.as_mut().set_album_info_loading(true);
        b.as_mut().set_album_info_error(QString::default());
        // Drop the previous document with the loading flag: a stale doc must
        // never paint under the spinner.
        b.as_mut().set_album_info_json(QString::from("{}"));
    });
    let runtime: Arc<AppRuntime<LoggingAdapter>> = crate::app();
    crate::spawn(async move {
        match runtime.core().get_album(&album_id).await {
            Ok(album) => {
                if generation == INFO_GENERATION.load(std::sync::atomic::Ordering::SeqCst) {
                    publish(&map(album, &album_id), false);
                }
            }
            Err(e) => {
                if generation != INFO_GENERATION.load(std::sync::atomic::Ordering::SeqCst) {
                    return;
                }
                log::warn!("[qbz-qt] album info load failed for {album_id}: {e}");
                let msg = e.to_string();
                album_bridge::ui(move |mut b| {
                    b.as_mut().set_album_info_loading(false);
                    b.as_mut().set_album_info_error(QString::from(msg.as_str()));
                });
            }
        }
    });
}

/// Monotonic open counter — see `open`.
static INFO_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[cfg(test)]
mod tests {
    use super::is_catalog_album_id;

    /// The regression. Every one of these is a real Qobuz album id: the first
    /// came out of the owner's own log (`cortinilla click 0: … id=vy32kidwrer67`),
    /// the rest out of the captured fixtures. The old `parse::<u64>()` guard
    /// rejected all four, and rejecting means the modal never gets a document.
    #[test]
    fn alphanumeric_qobuz_ids_are_catalog_ids() {
        for id in [
            "vy32kidwrer67",
            "cruwlxfl0d9ib",
            "ntn9yk9hil11t",
            "aev97320zm00a",
        ] {
            assert!(is_catalog_album_id(id), "{id} must reach /album/get");
        }
    }

    /// Barcode-shaped ids (the minority the old guard DID accept) still pass.
    #[test]
    fn barcode_shaped_ids_are_catalog_ids() {
        assert!(is_catalog_album_id("0886446573373"));
    }

    /// The backstop the guard actually exists for: local library paths and
    /// Plex keys, which the old numeric predicate let through.
    #[test]
    fn local_and_plex_keys_are_rejected() {
        assert!(!is_catalog_album_id(""));
        assert!(!is_catalog_album_id("plex:12345"));
        assert!(!is_catalog_album_id("/home/u/Music/Artist/Album"));
        assert!(!is_catalog_album_id("C:\\Music\\Album"));
        assert!(!is_catalog_album_id("Artist - Album (2021)"));
    }
}
