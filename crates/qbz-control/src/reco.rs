// crates/qbzd/src/api/reco.rs — POST /api/reco/playlist (CONSOLE extension).
// Playlist recommendations via QBZ's Suggested-Songs engine
// (core.generate_playlist_suggestions), enabled headlessly and Slint-free: the
// artist-vector store is opened at the daemon root at boot (daemon.rs), and the
// engine builds vectors on demand from MusicBrainz + Qobuz — NO listening
// history required, and the personalized (per-user history) recommendations
// stay out of scope (that would need play-recording plumbing).
//
// Body: {"playlist_id": N} — the daemon fetches the playlist, seeds the engine
// with its distinct artists and excludes its existing tracks; OR
// {"artists": [{"id"?: N, "name": "..."}], "exclude"?: [N,...]} for an explicit
// seed. Optional {"limit": N} caps the suggestion pool (1..=200).
//
// NOTE (single-thread cost): the engine resolves each seed artist against
// MusicBrainz (rate-limited) and fetches Qobuz tracks, so a large playlist can
// make this call take seconds; the API serving thread is inline (02 §3.1.1), so
// a long reco briefly blocks other requests. Acceptable for an infrequent,
// deliberate verb; offloading is a future improvement.
use std::collections::HashSet;
use std::io::Cursor;

use serde_json::Value;
use tiny_http::Response;

use qbz_models::FrontendAdapter;

use super::{err_json, json, CatalogContext};

pub fn playlist(
    state: &CatalogContext<'_, impl FrontendAdapter + 'static>,
    body: &Value,
) -> Response<Cursor<Vec<u8>>> {
    if let Some(resp) = auth_gate(state) {
        return resp;
    }

    let (artists, exclude) = if let Some(pid) = body.get("playlist_id").and_then(|v| v.as_u64()) {
        match seed_from_playlist(state, pid) {
            Ok(v) => v,
            Err(resp) => return resp,
        }
    } else if let Some(arr) = body.get("artists").and_then(|v| v.as_array()) {
        let exclude = body
            .get("exclude")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_u64()).collect())
            .unwrap_or_default();
        (parse_artists(arr), exclude)
    } else {
        return err_json(
            400,
            "bad_request",
            "reco requires playlist_id or artists",
            "body: {\"playlist_id\": 987} | {\"artists\": [{\"name\": \"Miles Davis\"}]}",
        );
    };

    if artists.is_empty() {
        return err_json(
            400,
            "bad_request",
            "no seed artists",
            "the playlist has no resolvable artists",
        );
    }

    let mut config = qbz_reco::SuggestionConfig::default();
    if let Some(n) = body.get("limit").and_then(|v| v.as_u64()) {
        config.max_pool_size = (n as usize).clamp(1, 200);
    }

    match state.rt.block_on(state.core.generate_playlist_suggestions(
        artists,
        exclude,
        true,
        Some(config),
    )) {
        Ok(res) => json(200, serde_json::to_value(res).unwrap_or(Value::Null)),
        Err(_) => err_json(
            502,
            "reco_failed",
            "suggestion engine failed",
            "try again in a moment; check: qbzd status",
        ),
    }
}

// ============================ internals ============================

/// Fetch a playlist and derive its distinct seed artists + exclude ids (its own
/// tracks). Mirrors the desktop's playlist-suggestions session
/// (qbz/src/playlist_suggestions.rs) but re-derived Slint-free from `Playlist`.
#[allow(clippy::type_complexity)]
fn seed_from_playlist(
    state: &CatalogContext<'_, impl FrontendAdapter + 'static>,
    pid: u64,
) -> Result<(Vec<(Option<u64>, String)>, Vec<u64>), Response<Cursor<Vec<u8>>>> {
    let pl = match state.rt.block_on(state.core.get_playlist(pid)) {
        Ok(p) => p,
        Err(_) => {
            return Err(err_json(
                404,
                "not_found",
                &format!("playlist {pid} not found"),
                "check: qbzd playlist list",
            ))
        }
    };
    let items = pl.tracks.map(|t| t.items).unwrap_or_default();
    let mut artists: Vec<(Option<u64>, String)> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut exclude: Vec<u64> = Vec::new();
    for t in &items {
        exclude.push(t.id);
        if let Some(p) = &t.performer {
            if !p.name.is_empty() && seen.insert(p.name.to_lowercase()) {
                artists.push((Some(p.id), p.name.clone()));
            }
        }
    }
    Ok((artists, exclude))
}

fn parse_artists(arr: &[Value]) -> Vec<(Option<u64>, String)> {
    arr.iter()
        .filter_map(|a| {
            let name = a
                .get("name")
                .and_then(|v| v.as_str())
                .filter(|n| !n.is_empty())?
                .to_string();
            let id = a.get("id").and_then(|v| v.as_u64());
            Some((id, name))
        })
        .collect()
}

fn auth_gate(
    state: &CatalogContext<'_, impl FrontendAdapter + 'static>,
) -> Option<Response<Cursor<Vec<u8>>>> {
    let needs_auth = state.needs_auth;
    if needs_auth {
        Some(err_json(
            409,
            "needs_auth",
            "not logged in to Qobuz",
            "run: qbzd login",
        ))
    } else {
        None
    }
}
