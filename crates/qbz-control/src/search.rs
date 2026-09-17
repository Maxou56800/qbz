// crates/qbzd/src/api/search.rs — GET /api/search (02-cli-and-api.md §2.3/§3.4
// row 19, P1). The first "originate playback" read verb: qbzd can now FIND
// music by name, not only receive it via QConnect or take track ids by hand.
//
// Server-side shaping (stable, typed contract): typed searches return the
// core's `SearchResultsPage<T>` (qbz-models/src/types.rs:811) verbatim under a
// per-category key; `type=all` runs the four typed searches and assembles them
// under one envelope. Reusing the shipped serde shapes keeps `--json` a frozen
// machine surface (§3.1.4) instead of leaking `catalog_search`'s raw Qobuz JSON.
//
// Blacklist filtering is intentionally NOT applied — the daemon opens no
// blacklist store (fail-open by design, qbz-core/src/core.rs:127-143); a
// documented GUI-parity delta (results may include items the desktop hides).
//
// Auth: gates on `NeedsAuth` exactly like `queue::add` — the typed searches
// call the Qobuz client, which is `CoreError::NotInitialized` without a session,
// so a needs-auth daemon answers 409 (→ CLI exit 4) rather than a bare failure.
use std::io::Cursor;

use serde_json::Value;
use tiny_http::Response;

use qbz_models::FrontendAdapter;

use super::{err_json, json, CatalogContext};

/// Default result count per category when the caller gives no `limit`.
const DEFAULT_LIMIT: u32 = 20;
/// Upper bound on `limit` — a control-plane search is top-hits, not a pager
/// (deep paging belongs in the GUI). Silently clamped, never a 400.
const MAX_LIMIT: u32 = 100;

/// `GET /api/search?q=&type=all|albums|tracks|artists|playlists&limit=&offset=`.
/// `query` is the raw query string (no leading `?`); `route()` strips it off the
/// path before dispatch. Errors: 409 `needs_auth`, 400 `bad_request` (missing
/// query / unknown type), 502 `search_failed` (upstream Qobuz error).
pub fn search(
    state: &CatalogContext<'_, impl FrontendAdapter + 'static>,
    query: &str,
) -> Response<Cursor<Vec<u8>>> {
    if let Some(resp) = auth_gate(state) {
        return resp;
    }

    let params = match parse_query(query) {
        Ok(p) => p,
        Err((message, hint)) => return err_json(400, "bad_request", &message, &hint),
    };

    // Which categories to fetch — `all` fans out to the four typed searches;
    // a typed request populates exactly one key (the rest stay `null`).
    let (do_albums, do_tracks, do_artists, do_playlists) = match params.stype {
        SearchType::All => (true, true, true, true),
        SearchType::One(Category::Albums) => (true, false, false, false),
        SearchType::One(Category::Tracks) => (false, true, false, false),
        SearchType::One(Category::Artists) => (false, false, true, false),
        SearchType::One(Category::Playlists) => (false, false, false, true),
    };

    let mut albums = Value::Null;
    let mut tracks = Value::Null;
    let mut artists = Value::Null;
    let mut playlists = Value::Null;

    if do_albums {
        match state.rt.block_on(state.core.search_albums(
            &params.q,
            params.limit,
            params.offset,
            None,
        )) {
            Ok(page) => albums = serde_json::to_value(page).unwrap_or(Value::Null),
            Err(_) => return upstream_error(),
        }
    }
    if do_tracks {
        match state.rt.block_on(state.core.search_tracks(
            &params.q,
            params.limit,
            params.offset,
            None,
        )) {
            Ok(page) => tracks = serde_json::to_value(page).unwrap_or(Value::Null),
            Err(_) => return upstream_error(),
        }
    }
    if do_artists {
        match state.rt.block_on(state.core.search_artists(
            &params.q,
            params.limit,
            params.offset,
            None,
        )) {
            Ok(page) => artists = serde_json::to_value(page).unwrap_or(Value::Null),
            Err(_) => return upstream_error(),
        }
    }
    if do_playlists {
        match state.rt.block_on(
            state
                .core
                .search_playlists(&params.q, params.limit, params.offset),
        ) {
            Ok(page) => playlists = serde_json::to_value(page).unwrap_or(Value::Null),
            Err(_) => return upstream_error(),
        }
    }

    json(
        200,
        serde_json::json!({
            "query": params.q,
            "type": params.stype.as_str(),
            "limit": params.limit,
            "offset": params.offset,
            "albums": albums,
            "tracks": tracks,
            "artists": artists,
            "playlists": playlists,
        }),
    )
}

// ============================ internals ============================

/// 409 `needs_auth` — search needs a live Qobuz session. Mirrors
/// `queue::add`'s gate (this file's self-contained-helpers convention).
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

/// 502 for any upstream Qobuz/core failure. The code maps to CLI exit 1
/// (`error_from_envelope`'s catch-all), never a panic. The `CoreError` is not
/// interpolated (matches `queue::add`'s `Err(_)` discipline — the daemon does
/// not leak upstream error text through the control plane).
fn upstream_error() -> Response<Cursor<Vec<u8>>> {
    err_json(
        502,
        "search_failed",
        "search request to Qobuz failed",
        "try again in a moment; check: qbzd status",
    )
}

/// The four searchable categories (`type=<one>`), plus the `all` fan-out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Category {
    Albums,
    Tracks,
    Artists,
    Playlists,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchType {
    All,
    One(Category),
}

impl SearchType {
    /// The canonical `type` string echoed back in the response (matches the CLI
    /// flag values 1:1 so a script round-trips `--type` through `--json`).
    fn as_str(self) -> &'static str {
        match self {
            SearchType::All => "all",
            SearchType::One(Category::Albums) => "albums",
            SearchType::One(Category::Tracks) => "tracks",
            SearchType::One(Category::Artists) => "artists",
            SearchType::One(Category::Playlists) => "playlists",
        }
    }
}

#[derive(Debug)]
struct SearchParams {
    q: String,
    stype: SearchType,
    limit: u32,
    offset: u32,
}

/// Parse `q`/`query` (percent-decoded), `type` (strict literal, default `all`),
/// `limit` (clamped 1..=MAX, default 20), `offset` (default 0). A missing/blank
/// query is a 400 (§1.4 error voice); an unknown `type` is a 400; malformed
/// numeric params degrade to defaults (a read route never 400s on a bad number,
/// mirroring `queue::parse_offset_limit`).
fn parse_query(query: &str) -> Result<SearchParams, (String, String)> {
    let mut q: Option<String> = None;
    let mut stype = SearchType::All;
    let mut limit = DEFAULT_LIMIT;
    let mut offset = 0u32;

    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let mut kv = pair.splitn(2, '=');
        let key = kv.next().unwrap_or("");
        let val = kv.next().unwrap_or("");
        match key {
            "q" | "query" => {
                let decoded = urlencoding::decode(val)
                    .map(|c| c.into_owned())
                    .unwrap_or_else(|_| val.to_string());
                q = Some(decoded);
            }
            "type" => {
                stype = match val {
                    "all" | "" => SearchType::All,
                    "albums" => SearchType::One(Category::Albums),
                    "tracks" => SearchType::One(Category::Tracks),
                    "artists" => SearchType::One(Category::Artists),
                    "playlists" => SearchType::One(Category::Playlists),
                    other => {
                        return Err((
                            format!("unknown type '{other}'"),
                            "type: all | albums | tracks | artists | playlists".into(),
                        ))
                    }
                };
            }
            "limit" => {
                if let Ok(n) = val.parse::<u32>() {
                    limit = n.clamp(1, MAX_LIMIT);
                }
            }
            "offset" => {
                if let Ok(n) = val.parse::<u32>() {
                    offset = n;
                }
            }
            _ => {}
        }
    }

    let q = match q {
        Some(q) if !q.trim().is_empty() => q,
        _ => {
            return Err((
                "search requires a query".into(),
                "usage: qbzd search <QUERY>".into(),
            ))
        }
    };
    Ok(SearchParams {
        q,
        stype,
        limit,
        offset,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_query_defaults_type_all_limit_20_offset_0() {
        let p = parse_query("q=spain").unwrap();
        assert_eq!(p.q, "spain");
        assert_eq!(p.stype, SearchType::All);
        assert_eq!(p.limit, 20);
        assert_eq!(p.offset, 0);
    }

    #[test]
    fn parse_query_percent_decodes_the_query() {
        // `light as a feather` → space-encoded both ways.
        let p = parse_query("q=light%20as%20a%20feather").unwrap();
        assert_eq!(p.q, "light as a feather");
        let plus = parse_query("q=kind+of+blue").unwrap();
        // urlencoding treats `+` literally (RFC 3986); a real client encodes
        // spaces as %20, so `+` stays a plus — documented, not a bug.
        assert_eq!(plus.q, "kind+of+blue");
    }

    #[test]
    fn parse_query_reads_each_typed_category() {
        assert_eq!(
            parse_query("q=x&type=albums").unwrap().stype,
            SearchType::One(Category::Albums)
        );
        assert_eq!(
            parse_query("q=x&type=tracks").unwrap().stype,
            SearchType::One(Category::Tracks)
        );
        assert_eq!(
            parse_query("q=x&type=artists").unwrap().stype,
            SearchType::One(Category::Artists)
        );
        assert_eq!(
            parse_query("q=x&type=playlists").unwrap().stype,
            SearchType::One(Category::Playlists)
        );
        assert_eq!(parse_query("q=x&type=all").unwrap().stype, SearchType::All);
    }

    #[test]
    fn parse_query_rejects_missing_or_blank_query() {
        assert!(parse_query("").is_err());
        assert!(parse_query("type=albums").is_err());
        assert!(parse_query("q=").is_err());
        assert!(parse_query("q=%20%20").is_err());
    }

    #[test]
    fn parse_query_rejects_unknown_type() {
        let (message, _hint) = parse_query("q=x&type=songs").unwrap_err();
        assert_eq!(message, "unknown type 'songs'");
    }

    #[test]
    fn parse_query_clamps_limit_and_ignores_bad_numbers() {
        assert_eq!(parse_query("q=x&limit=500").unwrap().limit, MAX_LIMIT);
        assert_eq!(parse_query("q=x&limit=0").unwrap().limit, 1);
        assert_eq!(parse_query("q=x&limit=nope").unwrap().limit, DEFAULT_LIMIT);
        assert_eq!(parse_query("q=x&offset=5").unwrap().offset, 5);
        assert_eq!(parse_query("q=x&offset=bad").unwrap().offset, 0);
    }

    #[test]
    fn search_type_as_str_round_trips_the_flag_values() {
        assert_eq!(SearchType::All.as_str(), "all");
        assert_eq!(SearchType::One(Category::Albums).as_str(), "albums");
        assert_eq!(SearchType::One(Category::Tracks).as_str(), "tracks");
        assert_eq!(SearchType::One(Category::Artists).as_str(), "artists");
        assert_eq!(SearchType::One(Category::Playlists).as_str(), "playlists");
    }
}
