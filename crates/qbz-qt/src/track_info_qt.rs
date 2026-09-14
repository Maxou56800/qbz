//! Track Info document for the now-playing bars' info button.
//!
//! Port of `qbz/src/info_modals.rs::map_track_info`. The credit parsing and
//! its role localization are frontend-agnostic already
//! (`qbz_qobuz::performers`, ADR-006), so this module only fetches the track
//! and shapes the document `qml/shell/TrackInfoModal.qml` parses.

use std::sync::Arc;

use qbz_app::shell::AppRuntime;
use qbz_core::LoggingAdapter;
use qbz_models::Track;
use qbz_qobuz::performers::{format_role_label, group_credits_ordered, parse_performers};
use serde::Serialize;

use crate::album_bridge;
use cxx_qt_lib::QString;

#[derive(Serialize)]
struct CreditRow {
    /// Display form, upper-cased like the Slint modal ("PRODUCER").
    role: String,
    /// The untranslated role, kept so the UI can group without re-parsing.
    #[serde(rename = "roleRaw")]
    role_raw: String,
    names: Vec<String>,
}

#[derive(Serialize, Default)]
struct TrackInfoDoc {
    title: String,
    album: String,
    artist: String,
    #[serde(rename = "artistId")]
    artist_id: String,
    duration: String,
    quality: String,
    isrc: String,
    label: String,
    #[serde(rename = "labelId")]
    label_id: String,
    copyright: String,
    credits: Vec<CreditRow>,
}

fn duration_text(secs: u32) -> String {
    let (m, s) = (secs / 60, secs % 60);
    format!("{m}:{s:02}")
}

fn map(track: Track) -> TrackInfoDoc {
    let (artist, artist_id) = match track.performer.as_ref() {
        Some(a) if a.id != 0 => (a.name.clone(), a.id.to_string()),
        Some(a) => (a.name.clone(), String::new()),
        None => (String::new(), String::new()),
    };
    let (label, label_id) = match track.album.as_ref().and_then(|a| a.label.as_ref()) {
        Some(l) => (l.name.clone(), l.id.to_string()),
        None => (String::new(), String::new()),
    };
    let credits = group_credits_ordered(&parse_performers(
        track.performers.as_deref().unwrap_or_default(),
    ))
    .into_iter()
    .map(|(role, names)| CreditRow {
        role: format_role_label(&role).to_uppercase(),
        role_raw: role,
        // Catalog fillers ("Not Documented") are not people (2026-09-13).
        names: names
            .into_iter()
            .filter(|name| !qbz_text_utils::names::is_placeholder_name(name))
            .collect(),
    })
    .filter(|row| !row.names.is_empty())
    .collect();

    TrackInfoDoc {
        title: track.title.clone(),
        album: track
            .album
            .as_ref()
            .map(|a| a.title.clone())
            .unwrap_or_default(),
        artist,
        artist_id,
        duration: duration_text(track.duration),
        // The same tier/detail string the quality stamp shows.
        quality: crate::quality_state::detail(track.maximum_bit_depth, track.maximum_sampling_rate),
        isrc: track.isrc.clone().unwrap_or_default(),
        label,
        label_id,
        copyright: track.copyright.clone().unwrap_or_default(),
        credits,
    }
}

fn publish(doc: &TrackInfoDoc, loading: bool) {
    let json = serde_json::to_string(doc).unwrap_or_else(|_| "{}".into());
    album_bridge::ui(move |mut b| {
        b.as_mut().set_track_info_json(QString::from(json.as_str()));
        b.as_mut().set_track_info_loading(loading);
    });
}

/// Info button: fetch the track and publish its document. A LOCAL or Plex id
/// is not a Qobuz catalog id, so it is rejected here rather than sent to the
/// API — the bars hide the button for those rows, this is the backstop.
pub fn open(track_id: String) {
    let Ok(id) = track_id.parse::<u64>() else {
        log::debug!("[qbz-qt] track info skipped for non-catalog id '{track_id}'");
        return;
    };
    album_bridge::ui(|mut b| b.as_mut().set_track_info_loading(true));
    let runtime: Arc<AppRuntime<LoggingAdapter>> = crate::app();
    crate::spawn(async move {
        match runtime.core().get_track(id).await {
            Ok(track) => publish(&map(track), false),
            Err(e) => {
                log::warn!("[qbz-qt] track info load failed for {id}: {e}");
                publish(&TrackInfoDoc::default(), false);
            }
        }
    });
}
