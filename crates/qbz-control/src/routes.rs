/// The counted P0 route table (02 §3.2) — 17, FINAL, this test now guards the
/// budget forever. A route exists only iff a shipped client calls it (§3.1.4).
/// T1-T6 landed the first 3; T7 added the 9 playback + now-playing routes
/// (rows 4-12); T8 added the 4 queue routes (rows 13-16); T11 (this task)
/// adds `/api/settings/reload` (row 17) — the inline
/// `route_table_matches_spec_count` test pins the number so the 68-routes
/// failure shape (a route with no caller) cannot creep back in.
pub const P0_ROUTES: &[(&str, &str)] = &[
    ("GET", "/api/ping"),
    ("GET", "/api/info"),
    ("GET", "/api/status"),
    ("GET", "/api/now-playing"),
    ("POST", "/api/playback/play"),
    ("POST", "/api/playback/pause"),
    ("POST", "/api/playback/toggle"),
    ("POST", "/api/playback/stop"),
    ("POST", "/api/playback/next"),
    ("POST", "/api/playback/previous"),
    ("POST", "/api/playback/seek"),
    ("POST", "/api/playback/volume"),
    ("GET", "/api/queue"),
    ("POST", "/api/queue/add"),
    ("POST", "/api/queue/remove"),
    ("POST", "/api/queue/clear"),
    ("POST", "/api/settings/reload"),
];

/// The P1 route table (02 §3.4) — the content-verb surface. SAME HARD RULE as
/// P0 (§3.1.4): a route exists only iff a shipped CLI/TUI client calls it, and
/// this table grows one row per verb+route pair in the SAME change as the verb.
/// Row 19 (this change): `GET /api/search` — caller: `qbzd search`. The
/// `p1_route_table_grows_only_with_a_shipped_caller` test pins the count and
/// forbids overlap with P0, so the 68-routes failure shape cannot creep in
/// through the P1 door either.
pub const P1_ROUTES: &[(&str, &str)] = &[
    ("GET", "/api/search"),
    ("POST", "/api/play"),
    ("GET", "/api/album"),
    ("GET", "/api/artist"),
    ("GET", "/api/similar"),
    ("GET", "/api/suggest"),
    ("GET", "/api/discover"),
    ("GET", "/api/lyrics"),
    ("POST", "/api/radio"),
    ("POST", "/api/reco/playlist"),
    ("POST", "/api/playback/shuffle"),
    ("POST", "/api/playback/repeat"),
    ("POST", "/api/queue/move"),
    ("POST", "/api/queue/jump"),
    ("POST", "/api/queue/stop-after"),
    ("GET", "/api/favorites"),
    ("POST", "/api/favorites/add"),
    ("POST", "/api/favorites/remove"),
    ("GET", "/api/playlists"),
    ("GET", "/api/playlist"),
    ("POST", "/api/playlist/create"),
    ("POST", "/api/playlist/update"),
    ("POST", "/api/playlist/delete"),
    ("POST", "/api/playlist/tracks/add"),
    ("POST", "/api/playlist/tracks/remove"),
    ("GET", "/api/events"),
    ("GET", "/api/artwork/current"),
];
