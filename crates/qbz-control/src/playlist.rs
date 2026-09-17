// crates/qbzd/src/api/playlist.rs — playlists (02 §2.3, §3.4 row 24). GET
// /api/playlists (the user's collection) and GET /api/playlist?id= (one
// playlist with its COMPLETE track list — get_playlist auto-pages server-side).
// Reads only in this slice; playlist CRUD (create/update/delete/tracks) is a
// later batch. Auth-gated; typed serde shapes verbatim.
use std::io::Cursor;

use serde_json::Value;
use tiny_http::Response;

use qbz_models::FrontendAdapter;

use super::{err_json, json, CatalogContext};

/// `GET /api/playlists` — the user's playlist collection.
pub fn list(
    state: &CatalogContext<'_, impl FrontendAdapter + 'static>,
) -> Response<Cursor<Vec<u8>>> {
    if let Some(resp) = auth_gate(state) {
        return resp;
    }
    match state.rt.block_on(state.core.get_user_playlists()) {
        Ok(pls) => json(
            200,
            serde_json::json!({"playlists": serde_json::to_value(pls).unwrap_or(Value::Null)}),
        ),
        Err(_) => err_json(
            502,
            "playlists_failed",
            "playlists request to Qobuz failed",
            "try again in a moment",
        ),
    }
}

/// `GET /api/playlist?id=<ID>` — one playlist with its full track list.
pub fn show(
    state: &CatalogContext<'_, impl FrontendAdapter + 'static>,
    query: &str,
) -> Response<Cursor<Vec<u8>>> {
    if let Some(resp) = auth_gate(state) {
        return resp;
    }
    let id = match id_param(query) {
        Some(id) => id,
        None => {
            return err_json(
                400,
                "bad_request",
                "playlist requires a numeric id",
                "usage: qbzd playlist show <ID>",
            )
        }
    };
    match state.rt.block_on(state.core.get_playlist(id)) {
        Ok(pl) => json(
            200,
            serde_json::json!({"playlist": serde_json::to_value(pl).unwrap_or(Value::Null)}),
        ),
        Err(_) => err_json(
            404,
            "not_found",
            &format!("playlist {id} not found"),
            "check: qbzd playlist list",
        ),
    }
}

/// `POST /api/playlist/create`. Body `{"name": "...", "description"?: "...",
/// "public"?: bool}`. Returns the created playlist.
pub fn create(
    state: &CatalogContext<'_, impl FrontendAdapter + 'static>,
    body: &Value,
) -> Response<Cursor<Vec<u8>>> {
    if let Some(resp) = auth_gate(state) {
        return resp;
    }
    let name = match body
        .get("name")
        .and_then(|v| v.as_str())
        .filter(|n| !n.trim().is_empty())
    {
        Some(n) => n,
        None => {
            return err_json(
                400,
                "bad_request",
                "create requires a name",
                "body: {\"name\": \"My Playlist\"}",
            )
        }
    };
    let desc = body.get("description").and_then(|v| v.as_str());
    let public = body
        .get("public")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    match state
        .rt
        .block_on(state.core.create_playlist(name, desc, public))
    {
        Ok(pl) => json(
            200,
            serde_json::json!({"playlist": serde_json::to_value(pl).unwrap_or(Value::Null)}),
        ),
        Err(_) => err_json(
            502,
            "playlists_failed",
            "playlist create failed",
            "try again in a moment",
        ),
    }
}

/// `POST /api/playlist/update`. Body `{"id": N, "name"?, "description"?,
/// "public"?}`. Only the present fields change.
pub fn update(
    state: &CatalogContext<'_, impl FrontendAdapter + 'static>,
    body: &Value,
) -> Response<Cursor<Vec<u8>>> {
    if let Some(resp) = auth_gate(state) {
        return resp;
    }
    let id = match body.get("id").and_then(|v| v.as_u64()) {
        Some(id) => id,
        None => {
            return err_json(
                400,
                "bad_request",
                "update requires an id",
                "body: {\"id\": 987, \"name\": \"...\"}",
            )
        }
    };
    let name = body.get("name").and_then(|v| v.as_str());
    let desc = body.get("description").and_then(|v| v.as_str());
    let public = body.get("public").and_then(|v| v.as_bool());
    match state
        .rt
        .block_on(state.core.update_playlist(id, name, desc, public))
    {
        Ok(pl) => json(
            200,
            serde_json::json!({"playlist": serde_json::to_value(pl).unwrap_or(Value::Null)}),
        ),
        Err(_) => err_json(
            502,
            "playlists_failed",
            "playlist update failed",
            "try again in a moment",
        ),
    }
}

/// `POST /api/playlist/delete`. Body `{"id": N}`. Deletes an owned playlist.
pub fn delete(
    state: &CatalogContext<'_, impl FrontendAdapter + 'static>,
    body: &Value,
) -> Response<Cursor<Vec<u8>>> {
    if let Some(resp) = auth_gate(state) {
        return resp;
    }
    let id = match body.get("id").and_then(|v| v.as_u64()) {
        Some(id) => id,
        None => {
            return err_json(
                400,
                "bad_request",
                "delete requires an id",
                "body: {\"id\": 987}",
            )
        }
    };
    match state.rt.block_on(state.core.delete_playlist(id)) {
        Ok(()) => json(200, serde_json::json!({"ok": true, "deleted": id})),
        Err(_) => err_json(
            502,
            "playlists_failed",
            "playlist delete failed",
            "try again in a moment",
        ),
    }
}

/// `POST /api/playlist/tracks/add`. Body `{"id": N, "track_ids": [...]}`.
pub fn tracks_add(
    state: &CatalogContext<'_, impl FrontendAdapter + 'static>,
    body: &Value,
) -> Response<Cursor<Vec<u8>>> {
    if let Some(resp) = auth_gate(state) {
        return resp;
    }
    let id = match body.get("id").and_then(|v| v.as_u64()) {
        Some(id) => id,
        None => {
            return err_json(
                400,
                "bad_request",
                "add requires a playlist id",
                "body: {\"id\": 987, \"track_ids\": [...]}",
            )
        }
    };
    let track_ids = match parse_ids(body) {
        Ok(ids) => ids,
        Err((m, h)) => return err_json(400, "bad_request", &m, &h),
    };
    match state
        .rt
        .block_on(state.core.add_tracks_to_playlist(id, &track_ids))
    {
        Ok(()) => json(
            200,
            serde_json::json!({"ok": true, "added": track_ids.len()}),
        ),
        Err(_) => err_json(
            502,
            "playlists_failed",
            "add to playlist failed",
            "try again in a moment",
        ),
    }
}

/// `POST /api/playlist/tracks/remove`. Body `{"id": N, "track_ids": [...]}` —
/// PLAIN track ids. The daemon resolves them to per-playlist `playlist_track_id`
/// row ids (the row-id trap, qbz-models Track.playlist_track_id) before calling
/// `remove_tracks_from_playlist`, so clients never touch row ids.
pub fn tracks_remove(
    state: &CatalogContext<'_, impl FrontendAdapter + 'static>,
    body: &Value,
) -> Response<Cursor<Vec<u8>>> {
    if let Some(resp) = auth_gate(state) {
        return resp;
    }
    let id = match body.get("id").and_then(|v| v.as_u64()) {
        Some(id) => id,
        None => {
            return err_json(
                400,
                "bad_request",
                "remove requires a playlist id",
                "body: {\"id\": 987, \"track_ids\": [...]}",
            )
        }
    };
    let track_ids = match parse_ids(body) {
        Ok(ids) => ids,
        Err((m, h)) => return err_json(400, "bad_request", &m, &h),
    };
    let pl = match state.rt.block_on(state.core.get_playlist(id)) {
        Ok(p) => p,
        Err(_) => {
            return err_json(
                404,
                "not_found",
                &format!("playlist {id} not found"),
                "check: qbzd playlist list",
            )
        }
    };
    let wanted: std::collections::HashSet<u64> = track_ids.iter().copied().collect();
    let items = pl.tracks.map(|t| t.items).unwrap_or_default();
    let row_ids: Vec<u64> = items
        .iter()
        .filter(|t| wanted.contains(&t.id))
        .filter_map(|t| t.playlist_track_id)
        .collect();
    if row_ids.is_empty() {
        return err_json(
            404,
            "not_found",
            "none of those tracks are in the playlist",
            "check: qbzd playlist show <ID>",
        );
    }
    match state
        .rt
        .block_on(state.core.remove_tracks_from_playlist(id, &row_ids))
    {
        Ok(()) => json(
            200,
            serde_json::json!({"ok": true, "removed": row_ids.len()}),
        ),
        Err(_) => err_json(
            502,
            "playlists_failed",
            "remove from playlist failed",
            "try again in a moment",
        ),
    }
}

// ============================ internals ============================

/// Strict `track_ids` array parse (any non-u64 element is a 400).
fn parse_ids(body: &Value) -> Result<Vec<u64>, (String, String)> {
    let hint = "body: {\"id\": 987, \"track_ids\": [176544871]}".to_string();
    let arr = match body.get("track_ids").and_then(|v| v.as_array()) {
        Some(a) if !a.is_empty() => a,
        Some(_) => return Err(("'track_ids' must not be empty".into(), hint)),
        None => return Err(("requires a 'track_ids' array".into(), hint)),
    };
    let mut ids = Vec::with_capacity(arr.len());
    for (i, v) in arr.iter().enumerate() {
        match v.as_u64() {
            Some(id) => ids.push(id),
            None => return Err((format!("track_ids[{i}] is not an unsigned integer"), hint)),
        }
    }
    Ok(ids)
}

fn id_param(query: &str) -> Option<u64> {
    for pair in query.split('&') {
        let mut kv = pair.splitn(2, '=');
        if kv.next() == Some("id") {
            return kv.next().and_then(|v| v.parse::<u64>().ok());
        }
    }
    None
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn id_param_reads_numeric_id() {
        assert_eq!(id_param("id=987654"), Some(987654));
        assert_eq!(id_param("foo=1&id=42"), Some(42));
        assert_eq!(id_param("id=abc"), None);
        assert_eq!(id_param(""), None);
    }

    #[test]
    fn parse_ids_accepts_valid_and_rejects_bad() {
        assert_eq!(
            parse_ids(&serde_json::json!({"track_ids": [1, 2, 3]})),
            Ok(vec![1, 2, 3])
        );
        assert!(parse_ids(&serde_json::json!({"track_ids": []})).is_err());
        assert!(parse_ids(&serde_json::json!({"track_ids": [1, "x"]})).is_err());
        assert!(parse_ids(&serde_json::json!({})).is_err());
    }
}
