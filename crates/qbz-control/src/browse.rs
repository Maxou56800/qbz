// crates/qbzd/src/api/browse.rs — the catalog READ verbs (02 §2.3):
// GET /api/album, /api/artist, /api/similar, /api/suggest.
//
// All auth-gated (they call the Qobuz client → NotInitialized without a
// session), all return the core's typed serde shapes verbatim (a stable
// `--json` contract, §3.1.4 — no raw catalog_search leakage), all blacklist
// fail-open by design (the daemon opens no blacklist store; a documented
// GUI-parity delta). None mutate playback — they are pure reads that feed the
// composition pipeline (`--ids` on the CLI side pipes into `queue add -`).
use std::collections::HashMap;
use std::io::Cursor;

use serde_json::Value;
use tiny_http::Response;

use qbz_models::FrontendAdapter;

use super::{err_json, json, CatalogContext};

const DEFAULT_LIMIT: u32 = 20;
const MAX_LIMIT: u32 = 100;
/// Seed cap when `suggest` falls back to the current queue.
const QUEUE_SEED_CAP: usize = 20;

/// `GET /api/album?id=<ALBUM_ID>&suggest=<0|1>`. The full album envelope
/// (tracklist, ImageSet artwork, description, awards) via `core.get_album`;
/// `suggest=1` also includes similar albums (`get_album_suggest`).
pub fn album(
    state: &CatalogContext<'_, impl FrontendAdapter + 'static>,
    query: &str,
) -> Response<Cursor<Vec<u8>>> {
    if let Some(resp) = auth_gate(state) {
        return resp;
    }
    let p = parse(query);
    let id = match p.get("id") {
        Some(v) if !v.is_empty() => v.clone(),
        _ => {
            return err_json(
                400,
                "bad_request",
                "album requires an id",
                "usage: qbzd album <ALBUM_ID>",
            )
        }
    };

    let album = match state.rt.block_on(state.core.get_album(&id)) {
        Ok(a) => serde_json::to_value(a).unwrap_or(Value::Null),
        Err(_) => return not_found("album", &id),
    };
    let similar = if wants(&p, "suggest") {
        match state.rt.block_on(state.core.get_album_suggest(&id)) {
            Ok(s) => serde_json::to_value(s).unwrap_or(Value::Null),
            Err(_) => Value::Null,
        }
    } else {
        Value::Null
    };

    json(200, serde_json::json!({"album": album, "similar": similar}))
}

/// `GET /api/artist?id=<ID>&view=page|top|albums&limit=&offset=&release_type=`.
/// `page` (default) = the artist page (bio, top tracks, similar); `top` = the
/// full top-tracks list; `albums` = the paged releases grid.
pub fn artist(
    state: &CatalogContext<'_, impl FrontendAdapter + 'static>,
    query: &str,
) -> Response<Cursor<Vec<u8>>> {
    if let Some(resp) = auth_gate(state) {
        return resp;
    }
    let p = parse(query);
    let id = match p.get("id").and_then(|v| v.parse::<u64>().ok()) {
        Some(id) => id,
        None => {
            return err_json(
                400,
                "bad_request",
                "artist requires a numeric id",
                "usage: qbzd artist <ARTIST_ID>",
            )
        }
    };
    let (limit, offset) = limit_offset(&p);
    let view = p.get("view").map(String::as_str).unwrap_or("page");

    let core = state.core;
    match view {
        "page" => match state.rt.block_on(core.get_artist_page(id, None)) {
            Ok(r) => json(
                200,
                serde_json::json!({"view": "page", "page": serde_json::to_value(r).unwrap_or(Value::Null)}),
            ),
            Err(_) => not_found("artist", &id.to_string()),
        },
        "top" => match state.rt.block_on(core.get_artist_tracks(id, limit, offset)) {
            Ok(tc) => json(
                200,
                serde_json::json!({"view": "top", "tracks": serde_json::to_value(tc).unwrap_or(Value::Null)}),
            ),
            Err(_) => not_found("artist", &id.to_string()),
        },
        "albums" => {
            let release_type = p.get("release_type").map(String::as_str).unwrap_or("album");
            match state
                .rt
                .block_on(core.get_releases_grid(id, release_type, limit, offset, None))
            {
                Ok(r) => json(
                    200,
                    serde_json::json!({"view": "albums", "releases": serde_json::to_value(r).unwrap_or(Value::Null)}),
                ),
                Err(_) => not_found("artist", &id.to_string()),
            }
        }
        other => err_json(
            400,
            "bad_request",
            &format!("unknown view '{other}'"),
            "view: page | top | albums",
        ),
    }
}

/// `GET /api/similar?artist=<ID>` (similar artists) or `?album=<ID>` (similar
/// albums), `&limit=&offset=`. Exactly one selector.
pub fn similar(
    state: &CatalogContext<'_, impl FrontendAdapter + 'static>,
    query: &str,
) -> Response<Cursor<Vec<u8>>> {
    if let Some(resp) = auth_gate(state) {
        return resp;
    }
    let p = parse(query);
    let (limit, offset) = limit_offset(&p);

    if let Some(artist_id) = p.get("artist").and_then(|v| v.parse::<u64>().ok()) {
        return match state
            .rt
            .block_on(state.core.get_similar_artists(artist_id, limit, offset))
        {
            Ok(page) => json(
                200,
                serde_json::json!({"artists": serde_json::to_value(page).unwrap_or(Value::Null)}),
            ),
            Err(_) => not_found("artist", &artist_id.to_string()),
        };
    }
    if let Some(album_id) = p.get("album").filter(|v| !v.is_empty()) {
        return match state.rt.block_on(state.core.get_album_suggest(album_id)) {
            Ok(sug) => json(
                200,
                serde_json::json!({"albums": serde_json::to_value(sug).unwrap_or(Value::Null)}),
            ),
            Err(_) => not_found("album", album_id),
        };
    }
    err_json(
        400,
        "bad_request",
        "similar requires artist=<ID> or album=<ID>",
        "usage: qbzd similar artist:<ID> | album:<ID>",
    )
}

/// `GET /api/suggest?seed=<ID,ID,...>&limit=`. Dynamic For-You suggestions
/// (`get_dynamic_suggest`). Seeds are explicit (`seed=`) or, when omitted,
/// the current queue's track ids (current + upcoming, capped) — so the daemon
/// tracks no listening history, honoring the UNIX-honest seeding the CONSOLE
/// brief specifies.
pub fn suggest(
    state: &CatalogContext<'_, impl FrontendAdapter + 'static>,
    query: &str,
) -> Response<Cursor<Vec<u8>>> {
    if let Some(resp) = auth_gate(state) {
        return resp;
    }
    let p = parse(query);
    let (limit, _offset) = limit_offset(&p);

    let seeds: Vec<u64> = match p.get("seed") {
        Some(s) if !s.is_empty() => s
            .split(',')
            .filter_map(|x| x.trim().parse::<u64>().ok())
            .collect(),
        _ => {
            let q = state.rt.block_on(state.core.get_queue_state());
            let mut ids: Vec<u64> = Vec::new();
            if let Some(t) = &q.current_track {
                ids.push(t.id);
            }
            for t in q.upcoming.iter().take(QUEUE_SEED_CAP.saturating_sub(1)) {
                ids.push(t.id);
            }
            ids
        }
    };
    if seeds.is_empty() {
        return err_json(
            400,
            "bad_request",
            "suggest needs seed track ids",
            "play something first, or: qbzd suggest --seed <ID,ID>",
        );
    }

    match state
        .rt
        .block_on(state.core.get_dynamic_suggest(&seeds, limit))
    {
        Ok(tracks) => json(
            200,
            serde_json::json!({"tracks": serde_json::to_value(tracks).unwrap_or(Value::Null)}),
        ),
        Err(_) => err_json(
            502,
            "suggest_failed",
            "suggestion request to Qobuz failed",
            "try again in a moment",
        ),
    }
}

// ============================ internals ============================

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

fn not_found(kind: &str, id: &str) -> Response<Cursor<Vec<u8>>> {
    err_json(
        404,
        "not_found",
        &format!("{kind} {id} not found"),
        "check the id: qbzd search <QUERY>",
    )
}

/// Percent-decoded query-string map (values only; keys are plain ascii).
fn parse(query: &str) -> HashMap<String, String> {
    let mut m = HashMap::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let mut kv = pair.splitn(2, '=');
        let key = kv.next().unwrap_or("").to_string();
        let raw = kv.next().unwrap_or("");
        let val = urlencoding::decode(raw)
            .map(|c| c.into_owned())
            .unwrap_or_else(|_| raw.to_string());
        m.insert(key, val);
    }
    m
}

/// `limit` (clamped 1..=MAX, default 20) + `offset` (default 0).
fn limit_offset(p: &HashMap<String, String>) -> (u32, u32) {
    let limit = p
        .get("limit")
        .and_then(|v| v.parse::<u32>().ok())
        .map(|n| n.clamp(1, MAX_LIMIT))
        .unwrap_or(DEFAULT_LIMIT);
    let offset = p
        .get("offset")
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(0);
    (limit, offset)
}

/// A boolean-ish query flag (`?suggest=1` / `?suggest=true`).
fn wants(p: &HashMap<String, String>, key: &str) -> bool {
    matches!(p.get(key).map(String::as_str), Some("1") | Some("true"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_decodes_values_and_splits_pairs() {
        let m = parse("id=abc&view=top&q=a%20b");
        assert_eq!(m.get("id").unwrap(), "abc");
        assert_eq!(m.get("view").unwrap(), "top");
        assert_eq!(m.get("q").unwrap(), "a b");
    }

    #[test]
    fn limit_offset_clamps_and_defaults() {
        let mut m = HashMap::new();
        assert_eq!(limit_offset(&m), (DEFAULT_LIMIT, 0));
        m.insert("limit".into(), "500".into());
        m.insert("offset".into(), "5".into());
        assert_eq!(limit_offset(&m), (MAX_LIMIT, 5));
        m.insert("limit".into(), "0".into());
        assert_eq!(limit_offset(&m).0, 1);
    }

    #[test]
    fn wants_reads_truthy_flags() {
        let mut m = HashMap::new();
        assert!(!wants(&m, "suggest"));
        m.insert("suggest".into(), "1".into());
        assert!(wants(&m, "suggest"));
        m.insert("suggest".into(), "true".into());
        assert!(wants(&m, "suggest"));
        m.insert("suggest".into(), "0".into());
        assert!(!wants(&m, "suggest"));
    }
}
