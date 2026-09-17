//! Share links + system clipboard — the Qt port of `crates/qbz/src/share.rs`
//! (lines 1-70: the URL builders and the clipboard). Driven by the artist,
//! album, label, playlist and track Share actions.
//!
//! Both universal-link resolvers are live: album UPC -> Album.link and track
//! ISRC -> Song.link, with the Odesli URL path as their last-resort fallback.
//!
//! **The `open.` vs `play.` split is deliberate, not a typo.** Track and album
//! moved to `open.qobuz.com` for #514 (`share.rs:4`, `:14-16`, which records
//! that Tauri used `play.qobuz.com/album/{id}` and that the change is
//! intentional); playlist / artist / label stay on the web player at
//! `play.qobuz.com`. Normalising all five onto one host breaks parity.
//!
//! **Why the clipboard lives in Rust here.** cxx-qt-lib 0.7 exposes no
//! QClipboard binding, so QML's own idiom (an inactive `Loader` holding an
//! invisible `TextEdit`, `selectAll()` + `copy()` — `qml/rows/TrackRow.qml`)
//! is the only QML route; but only Rust can publish `QbzShell.toastJson`
//! (`toast_qt.rs`), and the reference arm raises a toast. Splitting the two
//! halves across the boundary would need a second invokable for no gain, so
//! the whole action is one Rust seam. The four existing QML clipboard sites
//! keep their idiom — this does not replace them.

// The five builders stay together on purpose: they are the application's
// complete URL surface. Keeping the set intact
// (rather than growing it one host at a time) is what stops a later arm from
// re-deriving a URL and reintroducing the `open.`/`play.` mix-up that
// `qml/views/LabelView.qml` already shipped once.

/// Canonical Qobuz track URL — the `open.qobuz.com` share form (#514).
/// `share.rs:4-7`.
pub(crate) fn qobuz_track_url(track_id: &str) -> String {
    format!("https://open.qobuz.com/track/{track_id}")
}

/// Qobuz web-player playlist URL (matches Tauri's share-playlist link).
/// `share.rs:9-12`.
pub(crate) fn qobuz_playlist_url(playlist_id: &str) -> String {
    format!("https://play.qobuz.com/playlist/{playlist_id}")
}

/// Qobuz album URL — the `open.qobuz.com` form (#514; Tauri's
/// `shareAlbumQobuzLink` used `https://play.qobuz.com/album/{id}`). Also
/// the source URL fed to Song.link for the album-level "Album.link".
/// `share.rs:14-19`.
pub(crate) fn qobuz_album_url(album_id: &str) -> String {
    format!("https://open.qobuz.com/album/{album_id}")
}

/// Qobuz web-player artist URL (header Share action). `share.rs:21-24`.
pub(crate) fn qobuz_artist_url(artist_id: &str) -> String {
    format!("https://play.qobuz.com/artist/{artist_id}")
}

/// Qobuz web-player label URL (label-page header Share action). There is no
/// Song.link/Album.link equivalent for labels — Qobuz-link only.
/// `share.rs:26-30`.
pub(crate) fn qobuz_label_url(label_id: &str) -> String {
    format!("https://play.qobuz.com/label/{label_id}")
}

unsafe extern "C" {
    fn qbz_clipboard_set_text(utf8: *const std::os::raw::c_char, len: i32);
}

/// Copy `text` to the system clipboard through Qt (`cxx/clipboard.cpp`).
///
/// #684: the arboard path before this needed `wlr/ext-data-control`, which
/// KWin has and Mutter does not; on GNOME it fell back to X11, which works
/// natively through XWayland and is unreachable inside the Flatpak sandbox —
/// every Copy link on GNOME + Flatpak logged "clipboard unavailable" and
/// copied nothing. QClipboard uses the base Wayland data-device protocol,
/// needs no sandbox permission, and QGuiApplication keeps the offer alive
/// for the life of the process (the #514 lifetime rule, without the
/// long-lived instance the arboard port had to carry).
///
/// Fire-and-forget by design: the caller never learns whether the copy
/// worked, which is why the toast at the call site is unconditional
/// (`main.rs:12758-12761` does the same). The C++ side queues the write onto
/// the GUI thread, so this is safe from a cxx-qt invokable or a worker.
pub(crate) fn copy_to_clipboard(text: String) {
    let len = i32::try_from(text.len()).unwrap_or(i32::MAX);
    // SAFETY: the pointer/length pair is read synchronously inside the call
    // (the C++ side copies into a QString before queueing), so `text` may be
    // dropped as soon as the call returns.
    unsafe { qbz_clipboard_set_text(text.as_ptr().cast(), len) };
}

/// Artist header ⋯ → Share. `artist/ArtistPageView.slint:530-538` fires
/// `media-action("artist", ArtistState.id, "share")`; the arm at
/// `crates/qbz/src/main.rs:12749-12763` copies `share::qobuz_artist_url(&id)`
/// and raises the "Link copied" success toast.
///
/// Link-ONLY: there is no Song.link/Odesli path for artists (see the module
/// header), so there is no "Fetching…" info toast either — the reference
/// shows one only for the track/album Song.link arms.
///
/// The empty-id guard is the reference's: `main.rs:12755` skips both the copy
/// and the toast when the id is empty, so a header that has not resolved yet
/// stays silent instead of copying `.../artist/`.
pub(crate) fn share_artist(artist_id: String) {
    if artist_id.is_empty() {
        return;
    }
    copy_to_clipboard(qobuz_artist_url(&artist_id));
    // "Link copied" is an existing msgid in all eight qbz-i18n catalogues.
    crate::toast_qt::success(qbz_i18n::t("Link copied"));
}

/// Label header ⋯ → Share. Same shape as `share_artist`, and the reference is
/// the same `media-action` table: `crates/qbz/src/main.rs:13164-13178` copies
/// `share::qobuz_label_url(&label_id)` and raises the "Link copied" success
/// toast. Link-only — the reference's own comment there says there is no
/// Song.link/Album.link equivalent for labels.
///
/// THIS REPLACES A BROKEN LINK, not just a missing toast. `LabelView.qml`
/// copied `https://open.qobuz.com/label/{id}` through a QML `TextEdit`, and
/// `open.qobuz.com` has NO `/label/` route — the copied URL simply did not
/// resolve. The host is per-ENTITY, not per-app (#514): track and album live
/// on `open.qobuz.com`, artist / playlist / label on `play.qobuz.com`. Owner-
/// confirmed 2026-08-02 with a working example, `https://play.qobuz.com/label/
/// 68307`. That asymmetry is why the five builders above are ported as a set:
/// a call site that formats its own URL gets this wrong, and did.
pub(crate) fn share_label(label_id: String) {
    if label_id.is_empty() {
        return;
    }
    copy_to_clipboard(qobuz_label_url(&label_id));
    crate::toast_qt::success(qbz_i18n::t("Link copied"));
}

// ---------------------------------------------------------------------------
// Album ⋯ menu Share rows (AlbumContextMenu.slint:120-135)
// ---------------------------------------------------------------------------

/// Album ⋯ → "Share Qobuz link": `main.rs:12245-12248` — copy + toast.
pub(crate) fn share_album_qobuz(album_id: String) {
    if album_id.is_empty() {
        return;
    }
    copy_to_clipboard(qobuz_album_url(&album_id));
    crate::toast_qt::success(qbz_i18n::t("Link copied"));
}

/// Album ⋯ → "Share Album.link" (`main.rs:12249-12280`): fetch the album for
/// its UPC, resolve UPC -> Deezer -> album.link, copy + toast. The Odesli
/// API cannot resolve Qobuz URLs (#514: 400 could_not_resolve_entity), so
/// the UPC path is the working one; songlink_url is the last-resort fallback.
pub(crate) fn share_album_link(album_id: String) {
    if album_id.is_empty() {
        return;
    }
    crate::toast_qt::info(qbz_i18n::t("Fetching Album.link..."));
    crate::spawn(async move {
        let upc = crate::app()
            .core()
            .get_album(&album_id)
            .await
            .ok()
            .and_then(|a| a.upc);
        match albumlink_for_album(&album_id, upc.as_deref()).await {
            Some(url) => {
                copy_to_clipboard(url);
                crate::toast_qt::success(qbz_i18n::t("Link copied"));
            }
            None => {
                log::warn!("[qbz-qt] Album.link resolution failed for {album_id}");
                crate::toast_qt::error(qbz_i18n::t("Failed to copy link"));
            }
        }
    });
}

/// Track context menu -> "Share Qobuz link": copy + success toast.
pub(crate) fn share_track_qobuz(track_id: String) {
    if track_id.is_empty() {
        return;
    }
    copy_to_clipboard(qobuz_track_url(&track_id));
    crate::toast_qt::success(qbz_i18n::t("Link copied"));
}

/// Track context menu -> "Share Song.link": fetch the ISRC, resolve it
/// through Deezer, and copy the universal URL.
pub(crate) fn share_track_link(track_id: String) {
    if track_id.is_empty() {
        return;
    }
    crate::toast_qt::info(qbz_i18n::t("Fetching Song.link..."));
    crate::spawn(async move {
        let isrc = match track_id.parse::<u64>() {
            Ok(id) => crate::app()
                .core()
                .get_track(id)
                .await
                .ok()
                .and_then(|track| track.isrc),
            Err(_) => None,
        };
        match songlink_for_track(&track_id, isrc.as_deref()).await {
            Some(url) => {
                copy_to_clipboard(url);
                crate::toast_qt::success(qbz_i18n::t("Link copied"));
            }
            None => {
                log::warn!("[qbz-qt] Song.link resolution failed for {track_id}");
                crate::toast_qt::error(qbz_i18n::t("Failed to copy link"));
            }
        }
    });
}

/// Playlist header Share action. Local playlists have no public Qobuz URL and
/// hide the action in QML; mixed playlists remain Qobuz playlists and share
/// their catalog id normally.
pub(crate) fn share_playlist(playlist_id: String) {
    if playlist_id.is_empty() || playlist_id.starts_with("local:") {
        return;
    }
    copy_to_clipboard(qobuz_playlist_url(&playlist_id));
    crate::toast_qt::success(qbz_i18n::t("Link copied"));
}

/// Shared HTTP client settings for the share resolvers (Tauri parity:
/// 10 s request / 5 s connect timeouts).
fn share_http_client() -> reqwest::Client {
    qbz_net_proxy::apply_current(reqwest::Client::builder())
        .map(|builder| {
            builder
                .timeout(std::time::Duration::from_secs(10))
                .connect_timeout(std::time::Duration::from_secs(5))
        })
        .and_then(|builder| builder.build().map_err(Into::into))
        .unwrap_or_default()
}

/// Resolve an ISRC/UPC code to a Deezer catalog id. `path` is
/// `track/isrc:{code}` or `album/upc:{code}`. Deezer answers misses with
/// HTTP 200 + an `{"error": ...}` body, so both shapes are checked.
async fn deezer_lookup(path: &str) -> Option<u64> {
    let url = format!("https://api.deezer.com/2.0/{path}");
    let resp = match share_http_client().get(&url).send().await {
        Ok(r) => r,
        Err(e) => {
            log::warn!("[qbz-qt] deezer lookup {path}: request failed: {e}");
            return None;
        }
    };
    if !resp.status().is_success() {
        log::warn!("[qbz-qt] deezer lookup {path}: HTTP {}", resp.status());
        return None;
    }
    let body: serde_json::Value = match resp.json().await {
        Ok(v) => v,
        Err(e) => {
            log::warn!("[qbz-qt] deezer lookup {path}: bad JSON: {e}");
            return None;
        }
    };
    if let Some(err) = body.get("error") {
        log::info!("[qbz-qt] deezer lookup {path}: no match ({err})");
        return None;
    }
    body.get("id").and_then(|v| v.as_u64())
}

/// Song.link page URL for a track — ISRC-first via Deezer. Odesli does not
/// accept Qobuz URLs as input, so the URL route is only a last resort when the
/// catalog response has no usable ISRC.
pub async fn songlink_for_track(track_id: &str, isrc: Option<&str>) -> Option<String> {
    if let Some(code) = isrc.map(str::trim).filter(|code| !code.is_empty()) {
        if let Some(deezer_id) = deezer_lookup(&format!("track/isrc:{code}")).await {
            log::info!("[qbz-qt] song.link via ISRC {code} -> deezer track {deezer_id}");
            return Some(format!("https://song.link/d/{deezer_id}"));
        }
    } else {
        log::info!("[qbz-qt] track {track_id} has no ISRC; trying Odesli URL fallback");
    }
    songlink_url(&qobuz_track_url(track_id)).await
}

/// Album.link page URL for an album — UPC-first via Deezer (#514). Qobuz
/// UPCs often carry a leading zero (13-digit EAN) while Deezer stores the
/// 12-digit form and does NOT match zero-padded input (verified), so a
/// trimmed retry is attempted.
pub async fn albumlink_for_album(album_id: &str, upc: Option<&str>) -> Option<String> {
    if let Some(code) = upc.map(str::trim).filter(|c| !c.is_empty()) {
        if let Some(deezer_id) = deezer_lookup(&format!("album/upc:{code}")).await {
            log::info!("[qbz-qt] album.link via UPC {code} -> deezer album {deezer_id}");
            return Some(format!("https://album.link/d/{deezer_id}"));
        }
        let trimmed = code.trim_start_matches('0');
        if trimmed != code && !trimmed.is_empty() {
            if let Some(deezer_id) = deezer_lookup(&format!("album/upc:{trimmed}")).await {
                log::info!(
                    "[qbz-qt] album.link via UPC {trimmed} (leading zeros trimmed) -> deezer album {deezer_id}"
                );
                return Some(format!("https://album.link/d/{deezer_id}"));
            }
        }
    } else {
        log::info!("[qbz-qt] album {album_id} has no UPC; trying Odesli URL fallback");
    }
    songlink_url(&qobuz_album_url(album_id)).await
}

/// Resolve a source URL to its universal Song.link (Odesli) page URL.
/// One GET to the Odesli API; returns the `pageUrl` field. NOTE: Odesli
/// cannot resolve Qobuz URLs (400 `could_not_resolve_entity`) — for Qobuz
/// content use the ISRC/UPC resolvers above; this remains as their last
/// resort and for any non-Qobuz source URL.
pub async fn songlink_url(source_url: &str) -> Option<String> {
    let resp = share_http_client()
        .get("https://api.song.link/v1-alpha.1/links")
        .query(&[("url", source_url), ("userCountry", "US")])
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        let snippet: String = body.chars().take(200).collect();
        log::warn!("[qbz-qt] song.link status {status} for {source_url}: {snippet}");
        return None;
    }
    let value: serde_json::Value = resp.json().await.ok()?;
    value
        .get("pageUrl")
        .and_then(|p| p.as_str())
        .map(|s| s.to_string())
}
