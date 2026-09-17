// crates/qbzd/src/api/lyrics.rs — GET /api/lyrics?id=<TRACK_ID|current>
// (02 §2.3 CONSOLE). Qobuz lyrics for a track, normalized to a stable shape:
//   {"track_id": N, "synced": bool, "lines": [{"time_ms"?: N, "text": "..."}]}
// The raw QobuzLyricsDocument is a tagged union (synced with per-line/word
// timestamps vs plain text); this flattens it to lines the CLI (and any client)
// can render without knowing the union. `id=current` resolves the queue cursor.
// Auth-gated; a track with no lyrics is a 200 with an empty `lines`, never a 404.
use std::io::Cursor;

use serde_json::Value;
use tiny_http::Response;

use qbz_qobuz::lyrics::{QobuzLyricsContent, QobuzLyricsDocument};

use qbz_models::FrontendAdapter;

use super::{err_json, json, CatalogContext};

pub fn lyrics(
    state: &CatalogContext<'_, impl FrontendAdapter + 'static>,
    query: &str,
) -> Response<Cursor<Vec<u8>>> {
    if let Some(resp) = auth_gate(state) {
        return resp;
    }

    let id = match resolve_id(state, query) {
        Ok(id) => id,
        Err(resp) => return resp,
    };

    match state.rt.block_on(state.core.get_lyrics(id)) {
        Ok(Some(doc)) => json(200, normalize(id, &doc)),
        Ok(None) => json(
            200,
            serde_json::json!({"track_id": id, "synced": false, "lines": []}),
        ),
        Err(_) => err_json(
            502,
            "lyrics_failed",
            "lyrics request to Qobuz failed",
            "try again in a moment",
        ),
    }
}

// ============================ internals ============================

/// Flatten the tagged lyrics union into `{track_id, synced, lines:[{time_ms?, text}]}`.
fn normalize(id: u64, doc: &QobuzLyricsDocument) -> Value {
    let (synced, lines): (bool, Vec<Value>) = match &doc.original {
        Some(QobuzLyricsContent::Synced { lines, .. }) => (
            true,
            lines
                .iter()
                .map(|l| match l.start {
                    Some(ms) => serde_json::json!({"time_ms": ms, "text": l.line}),
                    None => serde_json::json!({"text": l.line}),
                })
                .collect(),
        ),
        Some(QobuzLyricsContent::Plain { lines, .. }) => (
            false,
            lines
                .iter()
                .map(|l| serde_json::json!({"text": l.line}))
                .collect(),
        ),
        None => (false, Vec::new()),
    };
    serde_json::json!({"track_id": id, "synced": synced, "lines": lines})
}

/// `?id=<u64>` or `?id=current` (the queue cursor); default `current`.
fn resolve_id(
    state: &CatalogContext<'_, impl FrontendAdapter + 'static>,
    query: &str,
) -> Result<u64, Response<Cursor<Vec<u8>>>> {
    let raw = query
        .split('&')
        .filter_map(|p| {
            let mut kv = p.splitn(2, '=');
            (kv.next()? == "id").then(|| kv.next().unwrap_or(""))
        })
        .next()
        .unwrap_or("current");

    if raw.is_empty() || raw == "current" {
        return match state
            .rt
            .block_on(state.core.get_queue_state())
            .current_track
        {
            Some(t) => Ok(t.id),
            None => Err(err_json(
                404,
                "not_found",
                "nothing is playing",
                "give a track id: qbzd lyrics <TRACK_ID>",
            )),
        };
    }
    raw.parse::<u64>().map_err(|_| {
        err_json(
            400,
            "bad_request",
            "lyrics id must be a track id or 'current'",
            "usage: qbzd lyrics [TRACK_ID]",
        )
    })
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
