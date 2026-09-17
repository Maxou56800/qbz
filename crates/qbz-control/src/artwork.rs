use qbz_models::FrontendAdapter;
// crates/qbzd/src/api/artwork.rs — GET /api/artwork/current (CONSOLE ext).
//
// A stable redirect to the CURRENT track's cover art: a daemon-only client (a
// plasmoid, a bar applet, an `<img src>` in a dashboard) can point at
// `http://<host>/api/artwork/current` and always get the live cover without
// knowing the Qobuz CDN URL. The art URL is already stamped on the queue track
// (an unauthenticated CDN link), so this reads queue state and 302s to it — no
// Qobuz session required. `qbzd art` is the shipped CLI caller (§3.1.4).
use std::io::Cursor;

use tiny_http::{Header, Response};

use super::{err_json, CatalogContext};

/// `GET /api/artwork/current` → `302` to the current cover, or `404` when
/// nothing is playing / the track carries no art.
pub fn current(
    state: &CatalogContext<'_, impl FrontendAdapter + 'static>,
) -> Response<Cursor<Vec<u8>>> {
    let queue = state.rt.block_on(state.core.get_queue_state());
    let url = queue
        .current_track
        .as_ref()
        .and_then(|t| t.artwork_url.clone())
        .filter(|u| !u.is_empty());

    match url {
        Some(u) => match Header::from_bytes(&b"Location"[..], u.as_bytes()) {
            Ok(location) => Response::from_data(Vec::new())
                .with_status_code(302)
                .with_header(location),
            // A non-header-safe URL (control bytes) — never expected from the CDN.
            Err(_) => err_json(
                500,
                "internal",
                "artwork url is not a valid redirect target",
                "check: qbzd now",
            ),
        },
        None => err_json(
            404,
            "not_found",
            "no artwork for the current track",
            "is something playing?  qbzd now",
        ),
    }
}
