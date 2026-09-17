use std::io::Cursor;
use tiny_http::{Header, Response};

/// A pre-routing rejection. Carried as a small enum so [`access_gate`] stays a
/// pure decision (unit-testable without a tiny_http `Request`, which has no
/// public constructor) while `route` renders the normative envelope.
pub(crate) enum GateReject {
    OriginForbidden,
    InvalidToken,
}

impl GateReject {
    pub(crate) fn response(&self) -> Response<Cursor<Vec<u8>>> {
        match self {
            GateReject::OriginForbidden => err_json(
                403,
                "origin_forbidden",
                "requests with an Origin header are refused",
                "the control plane is not a browser API",
            ),
            GateReject::InvalidToken => err_json(
                401,
                "invalid_token",
                "missing or wrong bearer token",
                "set QBZD_TOKEN or check [server] token in qbzd.toml",
            ),
        }
    }
}

/// The pre-routing access decision (02 §3.1.2): Origin shield always on; the
/// opt-in Bearer required on every route except `GET /api/ping` when `token`
/// is `Some`. `None` = open (no auth machinery). Returns `Some(_)` to reject.
pub(crate) fn access_gate(
    has_origin: bool,
    method: &str,
    path: &str,
    auth_header: Option<&str>,
    token: Option<&str>,
) -> Option<GateReject> {
    if has_origin {
        return Some(GateReject::OriginForbidden);
    }
    if let Some(secret) = token {
        let is_ping = method == "GET" && path == "/api/ping";
        let expected = format!("Bearer {secret}");
        let ok = auth_header
            .map(|v| constant_time_eq(v.as_bytes(), expected.as_bytes()))
            .unwrap_or(false);
        if !is_ping && !ok {
            return Some(GateReject::InvalidToken);
        }
    }
    None
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Canonical JSON number for a volume level (0.0-1.0). `serde_json::Value`
/// backs f32 via `Number::from_f32`, which stores the value as `f as f64` —
/// the f32→f64 widening turns `0.8f32` into `0.800000011920929` on the wire.
/// 02-cli-and-api.md §2.2/§3.3.4 document plain `0.8`, and `--json` is the
/// frozen machine contract ("scripts parse this"), so every volume-bearing
/// response routes through this instead of a bare `json!(v)`. 3 decimals is
/// plenty of precision for a 0.0-1.0 level.
pub fn canon_volume(v: f32) -> serde_json::Value {
    let rounded = (v as f64 * 1000.0).round() / 1000.0;
    serde_json::json!(rounded)
}

/// A 2xx JSON response. `pub(crate)` so the per-route handlers in `status.rs`
/// share the exact same envelope framing.
pub fn json(status: u16, body: serde_json::Value) -> Response<Cursor<Vec<u8>>> {
    let bytes = serde_json::to_vec(&body).unwrap_or_else(|_| b"{}".to_vec());
    let header = Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..])
        .expect("static content-type header");
    Response::from_data(bytes)
        .with_status_code(status)
        .with_header(header)
}

/// The normative error envelope (02 §3.1.3): `{"error":{"code","message","hint"}}`.
/// The CLI keys its exit code off `code` (never raw HTTP status), and every hint
/// names the fix (§1.4 error voice). The G0 addendum's shorthand
/// `{"error":"origin_forbidden"}` is this same nested envelope — the uniform
/// §3.1.3 shape the CLI's `error_from_envelope` reads via `error.code`.
pub fn err_json(status: u16, code: &str, message: &str, hint: &str) -> Response<Cursor<Vec<u8>>> {
    json(status, error_body(code, message, hint))
}

pub fn error_body(code: &str, message: &str, hint: &str) -> serde_json::Value {
    serde_json::json!({"error": {"code": code, "message": message, "hint": hint}})
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{P0_ROUTES, P1_ROUTES};

    #[test]
    fn route_table_matches_spec_count() {
        // 02-cli-and-api.md §3.2 — P0 = exactly 17 routes, FINAL; grows ONLY
        // with a shipped client. T1-T6 landed 3; T7 +9 = 12; T8 +4 = 16; T11
        // (this task) +1 = 17.
        assert_eq!(P0_ROUTES.len(), 17);
        assert!(P0_ROUTES.contains(&("GET", "/api/ping")));
        assert!(P0_ROUTES.contains(&("GET", "/api/info")));
        assert!(P0_ROUTES.contains(&("GET", "/api/status")));
        assert!(P0_ROUTES.contains(&("GET", "/api/now-playing")));
        assert!(P0_ROUTES.contains(&("POST", "/api/playback/play")));
        assert!(P0_ROUTES.contains(&("POST", "/api/playback/pause")));
        assert!(P0_ROUTES.contains(&("POST", "/api/playback/toggle")));
        assert!(P0_ROUTES.contains(&("POST", "/api/playback/stop")));
        assert!(P0_ROUTES.contains(&("POST", "/api/playback/next")));
        assert!(P0_ROUTES.contains(&("POST", "/api/playback/previous")));
        assert!(P0_ROUTES.contains(&("POST", "/api/playback/seek")));
        assert!(P0_ROUTES.contains(&("POST", "/api/playback/volume")));
        assert!(P0_ROUTES.contains(&("GET", "/api/queue")));
        assert!(P0_ROUTES.contains(&("POST", "/api/queue/add")));
        assert!(P0_ROUTES.contains(&("POST", "/api/queue/remove")));
        assert!(P0_ROUTES.contains(&("POST", "/api/queue/clear")));
        assert!(P0_ROUTES.contains(&("POST", "/api/settings/reload")));
    }

    #[test]
    fn p1_route_table_grows_only_with_a_shipped_caller() {
        // 02-cli-and-api.md §3.4 — each P1 route lands with its CLI verb (the
        // §3.1.4 HARD RULE, applied to the content-verb door). Row 19:
        // GET /api/search — caller: `qbzd search`. Count is pinned so a route
        // with no caller cannot creep in; P1 must never overlap P0.
        assert_eq!(P1_ROUTES.len(), 27);
        assert!(P1_ROUTES.contains(&("GET", "/api/events"))); // caller: `qbzd watch`
        assert!(P1_ROUTES.contains(&("GET", "/api/artwork/current"))); // caller: `qbzd art`
        assert!(P1_ROUTES.contains(&("GET", "/api/discover")));
        assert!(P1_ROUTES.contains(&("GET", "/api/lyrics")));
        assert!(P1_ROUTES.contains(&("POST", "/api/reco/playlist")));
        assert!(P1_ROUTES.contains(&("GET", "/api/favorites")));
        assert!(P1_ROUTES.contains(&("POST", "/api/favorites/add")));
        assert!(P1_ROUTES.contains(&("POST", "/api/favorites/remove")));
        assert!(P1_ROUTES.contains(&("GET", "/api/playlists")));
        assert!(P1_ROUTES.contains(&("GET", "/api/playlist")));
        assert!(P1_ROUTES.contains(&("POST", "/api/playlist/create")));
        assert!(P1_ROUTES.contains(&("POST", "/api/playlist/update")));
        assert!(P1_ROUTES.contains(&("POST", "/api/playlist/delete")));
        assert!(P1_ROUTES.contains(&("POST", "/api/playlist/tracks/add")));
        assert!(P1_ROUTES.contains(&("POST", "/api/playlist/tracks/remove")));
        assert!(P1_ROUTES.contains(&("GET", "/api/search")));
        assert!(P1_ROUTES.contains(&("POST", "/api/play")));
        assert!(P1_ROUTES.contains(&("GET", "/api/album")));
        assert!(P1_ROUTES.contains(&("GET", "/api/artist")));
        assert!(P1_ROUTES.contains(&("GET", "/api/similar")));
        assert!(P1_ROUTES.contains(&("GET", "/api/suggest")));
        assert!(P1_ROUTES.contains(&("POST", "/api/radio")));
        assert!(P1_ROUTES.contains(&("POST", "/api/playback/shuffle")));
        assert!(P1_ROUTES.contains(&("POST", "/api/playback/repeat")));
        assert!(P1_ROUTES.contains(&("POST", "/api/queue/move")));
        assert!(P1_ROUTES.contains(&("POST", "/api/queue/jump")));
        assert!(P1_ROUTES.contains(&("POST", "/api/queue/stop-after")));
        for r in P1_ROUTES {
            assert!(
                !P0_ROUTES.contains(r),
                "{r:?} is duplicated across P0 and P1"
            );
        }
    }

    #[test]
    fn constant_time_eq_matches_only_identical_slices() {
        assert!(constant_time_eq(b"Bearer s3cret", b"Bearer s3cret"));
        assert!(!constant_time_eq(b"Bearer s3cret", b"Bearer wrong"));
        assert!(!constant_time_eq(b"Bearer s3cret", b"Bearer s3cret-extra"));
        assert!(!constant_time_eq(b"", b"x"));
    }

    fn code(r: Option<GateReject>) -> Option<&'static str> {
        r.map(|r| match r {
            GateReject::OriginForbidden => "origin_forbidden",
            GateReject::InvalidToken => "invalid_token",
        })
    }

    #[test]
    fn origin_header_is_refused_on_every_route_including_ping() {
        // Step 4(a): an Origin header → 403 origin_forbidden everywhere, even
        // /api/ping, and even in the open (token=None) default.
        for (m, p) in P0_ROUTES {
            assert_eq!(
                code(access_gate(true, m, p, None, None)),
                Some("origin_forbidden"),
                "{m} {p} with Origin must be refused"
            );
        }
        // ...and the Origin shield wins even when a valid Bearer is present.
        assert_eq!(
            code(access_gate(
                true,
                "GET",
                "/api/ping",
                Some("Bearer s3cret"),
                Some("s3cret")
            )),
            Some("origin_forbidden")
        );
    }

    #[test]
    fn open_mode_answers_every_route_without_auth() {
        // Step 4(b): token=None → no auth machinery; nothing is rejected.
        for (m, p) in P0_ROUTES {
            assert!(
                access_gate(false, m, p, None, None).is_none(),
                "{m} {p} must be open"
            );
        }
    }

    #[test]
    fn opt_in_token_rejects_missing_or_wrong_bearer_but_never_ping() {
        // Step 4(c): token=Some → missing/wrong bearer is 401 on non-ping routes;
        // /api/ping stays 200 (exempt); the correct bearer passes.
        let tok = Some("s3cret");
        assert_eq!(
            code(access_gate(false, "GET", "/api/status", None, tok)),
            Some("invalid_token")
        );
        assert_eq!(
            code(access_gate(
                false,
                "GET",
                "/api/status",
                Some("Bearer nope"),
                tok
            )),
            Some("invalid_token")
        );
        assert!(access_gate(false, "GET", "/api/status", Some("Bearer s3cret"), tok).is_none());
        // /api/ping answers even with no/ wrong bearer.
        assert!(access_gate(false, "GET", "/api/ping", None, tok).is_none());
        assert!(access_gate(false, "GET", "/api/ping", Some("Bearer nope"), tok).is_none());
    }

    #[test]
    fn canon_volume_pins_0_8_exactly_no_f32_widening() {
        // `serde_json::Number::from_f32` widens f32→f64 (`0.8f32` would
        // serialize raw as `0.800000011920929`); `canon_volume` must not.
        assert_eq!(serde_json::to_string(&canon_volume(0.8f32)).unwrap(), "0.8");
        assert_eq!(serde_json::to_string(&canon_volume(1.0f32)).unwrap(), "1.0");
        assert_eq!(serde_json::to_string(&canon_volume(0.0f32)).unwrap(), "0.0");
        assert_eq!(
            serde_json::to_string(&canon_volume(0.75f32)).unwrap(),
            "0.75"
        );
    }

    #[test]
    fn error_envelope_is_the_nested_shape_with_code() {
        // 02 §3.1.3 — the on-the-wire shape the CLI reads via `error.code`.
        let body = error_body("origin_forbidden", "refused", "not a browser API");
        assert_eq!(body["error"]["code"], "origin_forbidden");
        assert_eq!(body["error"]["message"], "refused");
        assert_eq!(body["error"]["hint"], "not a browser API");
    }
}
