//! Custom album covers and artist portraits + the artwork file actions — the
//! Qt port of `crates/qbz/src/custom_artwork.rs` and the cover/portrait menu
//! Rust arms (`main.rs:23167-23239` add/remove, `:23096-23159`
//! open-in-browser/save-as).
//!
//! The store is the SAME file and format the Slint app uses
//! (`<data-dir>/qbz/custom_artwork.json`, `albums: album_id → absolute path`
//! and `artists: artist NAME → absolute path`), so an override set in either
//! app shows in both. The store records the picked file's own path (Slint does
//! not copy/resize into the cache); if the user later moves or deletes that
//! file the override silently stops applying — the reference's accepted
//! behaviour, kept on purpose.
//!
//! The artist half is keyed by DISPLAY NAME, not id, because that is what
//! `artist/ArtistPageView.slint:312` writes. Two artists sharing a name
//! collide and an upstream rename orphans the override; "improving" this to
//! id-keying would stop the file round-tripping with the shipping Slint build,
//! which is the entire point of sharing it.

use std::collections::HashMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Default, Serialize, Deserialize)]
struct Store {
    /// Artist portraits, keyed by DISPLAY NAME (the Slint key — see the
    /// module header). Written by the artist page's portrait menu since the
    /// 2026-08-14 round; before that this port only round-tripped the map.
    #[serde(default)]
    artists: HashMap<String, String>,
    #[serde(default)]
    albums: HashMap<String, String>,
}

fn store_path() -> Option<PathBuf> {
    Some(dirs::data_dir()?.join("qbz").join("custom_artwork.json"))
}

fn load_store() -> Store {
    let Some(path) = store_path() else {
        return Store::default();
    };
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => Store::default(),
    }
}

fn write_store(store: &Store) {
    let Some(path) = store_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            log::warn!("[qbz-qt] custom-artwork dir failed: {e}");
            return;
        }
    }
    match serde_json::to_vec_pretty(store) {
        Ok(json) => {
            if let Err(e) = std::fs::write(&path, json) {
                log::warn!("[qbz-qt] custom-artwork write failed: {e}");
            }
        }
        Err(e) => log::warn!("[qbz-qt] custom-artwork serialize failed: {e}"),
    }
}

/// The absolute path of the user-picked cover for `album_id`, if registered.
/// `album_qt::load_album` calls this on every build to seed the header's
/// `hasCustomCover` / `customCoverPath`.
pub fn album_cover(album_id: &str) -> Option<String> {
    load_store().albums.get(album_id).cloned()
}

/// The art URL a card/collage builder should emit for an album: the custom
/// override's absolute path when one is registered AND the file still
/// exists, else the remote URL unchanged. The path flows through the
/// ordinary artwork pipeline (artwork_qt::classify's LocalFile arm resolves
/// it synchronously), so every surface that mounts AlbumCard picks the
/// override up with no QML change.
pub fn prefer_album_cover(album_id: &str, fallback_url: String) -> String {
    match album_cover(album_id) {
        Some(p) if std::path::Path::new(&p).is_file() => p,
        _ => fallback_url,
    }
}

/// A one-shot snapshot of BOTH override maps, for a card/collage builder that
/// resolves many rows at once (Home's `attach_cached` runs over hundreds of
/// cards): read the store ONCE here instead of per row. `is_empty()` is the
/// fast path — a profile with no overrides pays a single empty read and skips.
pub struct OverrideSnapshot {
    albums: HashMap<String, String>,
    artists: HashMap<String, String>,
}

impl OverrideSnapshot {
    pub fn is_empty(&self) -> bool {
        self.albums.is_empty() && self.artists.is_empty()
    }
    /// Album override path (if registered AND on disk), else `fallback`.
    pub fn album(&self, album_id: &str, fallback: String) -> String {
        match self.albums.get(album_id) {
            Some(p) if std::path::Path::new(p).is_file() => p.clone(),
            _ => fallback,
        }
    }
    /// Artist portrait override path (keyed by NAME), else `fallback`.
    pub fn artist(&self, name: &str, fallback: String) -> String {
        match self.artists.get(name) {
            Some(p) if std::path::Path::new(p).is_file() => p.clone(),
            _ => fallback,
        }
    }
}

/// Snapshot both override maps in one store read.
pub fn override_snapshot() -> OverrideSnapshot {
    let store = load_store();
    OverrideSnapshot {
        albums: store.albums,
        artists: store.artists,
    }
}

fn set_album_cover(album_id: &str, path: &str) {
    let mut store = load_store();
    store.albums.insert(album_id.to_string(), path.to_string());
    write_store(&store);
}

fn remove_album_cover(album_id: &str) {
    let mut store = load_store();
    if store.albums.remove(album_id).is_some() {
        write_store(&store);
    }
}

/// "Add cover" / "Change cover": native picker, persist the picked path,
/// then re-open the album so the header repaints from the override.
/// Cancel = no-op, no toast (the picker's own affordance).
pub fn add_custom_cover(album_id: String, artwork_url: String) {
    if album_id.is_empty() {
        return;
    }
    crate::spawn(async move {
        let Some(file) = rfd::AsyncFileDialog::new()
            .add_filter("Images", &["png", "jpg", "jpeg", "webp"])
            .pick_file()
            .await
        else {
            return;
        };
        let path = file.path().to_string_lossy().to_string();
        // Blocking IO, but tiny (read + write of a small JSON map); the spawn
        // keeps it off the Qt thread, same as the reference's handler.
        let id = album_id.clone();
        // The hash -> override link is what lets the rest of the app (cards,
        // NPB, queue, mosaics) render this art for this album.
        note_override_key(&artwork_url, &path);
        if tokio::task::spawn_blocking(move || {
            set_album_cover(&id, &path);
        })
        .await
        .is_ok()
        {
            crate::open_album(album_id);
        }
    });
}

/// "Remove cover": drop the override and re-open so the header reverts to
/// the Qobuz artwork.
pub fn remove_custom_cover(album_id: String) {
    if album_id.is_empty() {
        return;
    }
    let id = album_id.clone();
    crate::spawn(async move {
        let _ = tokio::task::spawn_blocking(move || remove_album_cover(&id)).await;
        crate::open_album(album_id);
    });
}

/// "Save as…": native save dialog seeded `{title} - Cover.jpg`. The source
/// is the custom override when one is set, else the pipeline's cached file
/// for the header URL, else one GET of the URL (Slint `main.rs:23108-23159`).
pub fn save_cover_as(album_id: String, title: String, artwork_url: String) {
    crate::spawn(async move {
        let default_name = format!(
            "{} - Cover.jpg",
            if title.is_empty() { "album" } else { &title }
        );
        let Some(dest) = rfd::AsyncFileDialog::new()
            .set_file_name(&default_name)
            .add_filter("JPEG", &["jpg", "jpeg"])
            .save_file()
            .await
        else {
            return;
        };
        let dest_path = dest.path().to_path_buf();

        // Local sources first: the override file, then the artwork cache.
        let local = album_cover(&album_id)
            .filter(|p| !p.is_empty())
            .or_else(|| {
                let cached = crate::artwork_qt::cached_path(&artwork_url);
                if cached.is_empty() {
                    None
                } else {
                    Some(cached)
                }
            });
        if let Some(src) = local {
            if let Err(e) = tokio::fs::copy(&src, &dest_path).await {
                log::warn!("[qbz-qt] cover save-as copy failed: {e}");
            }
            return;
        }
        if artwork_url.is_empty() {
            return;
        }
        let builder = match qbz_net_proxy::apply_current(reqwest::Client::builder()) {
            Ok(b) => b,
            Err(e) => {
                log::warn!("[qbz-qt] cover save-as proxy config error: {e}");
                return;
            }
        };
        let client = match builder.timeout(std::time::Duration::from_secs(30)).build() {
            Ok(c) => c,
            Err(e) => {
                log::warn!("[qbz-qt] cover save-as client error: {e}");
                return;
            }
        };
        match client
            .get(&artwork_url)
            .send()
            .await
            .and_then(|r| r.error_for_status())
        {
            Ok(resp) => match resp.bytes().await {
                Ok(bytes) => {
                    if let Err(e) = tokio::fs::write(&dest_path, &bytes).await {
                        log::warn!("[qbz-qt] cover save-as write failed: {e}");
                    }
                }
                Err(e) => log::warn!("[qbz-qt] cover save-as read failed: {e}"),
            },
            Err(e) => log::warn!("[qbz-qt] cover save-as fetch failed: {e}"),
        }
    });
}

// ---------------------------------------------------------------------------
// Artist portraits (the artist half of the SHARED store)
// ---------------------------------------------------------------------------
// 1:1 with `crates/qbz/src/custom_artwork.rs:69-86` — same file, same map,
// same NAME key, so a portrait set in either build shows in both.

/// The absolute path of the user-picked portrait for `name`, if registered.
/// `artist_qt::load_artist` calls this on every build to seed the header's
/// `hasCustomImage` / `customImagePath`.
pub fn artist_image(name: &str) -> Option<String> {
    load_store().artists.get(name).cloned()
}

/// The image URL a card/collage builder should emit for an artist: the custom
/// portrait's absolute path when one is registered AND the file still exists,
/// else the remote URL unchanged. The twin of [`prefer_album_cover`] — every
/// surface that mounts an artist card (Home, Library, Search) picks the
/// override up with no QML change, so a portrait set on the artist page also
/// shows on its cards.
pub fn prefer_artist_image(name: &str, fallback_url: String) -> String {
    match artist_image(name) {
        Some(p) if std::path::Path::new(&p).is_file() => p,
        _ => fallback_url,
    }
}

fn set_artist_image(name: &str, path: &str) {
    let mut store = load_store();
    store.artists.insert(name.to_string(), path.to_string());
    write_store(&store);
}

fn remove_artist_image(name: &str) {
    let mut store = load_store();
    if store.artists.remove(name).is_some() {
        write_store(&store);
    }
}

/// "Add image" / "Change image": native picker, persist the picked path, then
/// repaint the OPEN artist page.
///
/// The repaint is `artist_qt::apply_custom_image`, NOT `crate::open_artist`:
/// that router returns early when the session is offline (`main.rs:1071`), so
/// re-opening would leave a portrait picked offline invisible until a restart;
/// it also records a second nav entry (Back would land on the same artist) and
/// feeds the search-ranking interaction log with a click the user never made.
/// The Slint reference has the same instinct for a different reason — it
/// applies the decoded image in place without a reload
/// (`crates/qbz/src/main.rs:23184-23187`) precisely because the artists this
/// feature exists for are the ones Qobuz has no portrait for.
pub fn add_custom_artist_image(artist_name: String, artwork_url: String) {
    if artist_name.is_empty() {
        return;
    }
    crate::spawn(async move {
        let Some(file) = rfd::AsyncFileDialog::new()
            .add_filter("Images", &["png", "jpg", "jpeg", "webp"])
            .pick_file()
            .await
        else {
            return;
        };
        let path = file.path().to_string_lossy().to_string();
        // The hash -> override link is what carries this portrait to the rest
        // of the app (search cards, similar-artist rails, the sidebar dock).
        // It is a no-op when the artist has no Qobuz portrait to key on — that
        // artist's override then applies on the artist page alone, which is
        // still strictly better than today's nothing.
        note_override_key(&artwork_url, &path);
        let name = artist_name.clone();
        if tokio::task::spawn_blocking(move || {
            set_artist_image(&name, &path);
        })
        .await
        .is_ok()
        {
            crate::artist_qt::apply_custom_image(&artist_name);
        }
    });
}

/// "Remove image": drop the override (and its hash link) and repaint, so the
/// header reverts to the Qobuz portrait.
///
/// Dropping the hash link is what the album twin cannot do — `cover_remove_
/// custom` takes only the album id and so has no url to compute the key from,
/// which leaves a removed album cover still overriding every card. Fixing that
/// means widening an invokable this lane does not own; the artist arm is built
/// without the hole rather than made symmetric with it.
pub fn remove_custom_artist_image(artist_name: String, artwork_url: String) {
    if artist_name.is_empty() {
        return;
    }
    let name = artist_name.clone();
    let url = artwork_url.clone();
    crate::spawn(async move {
        let _ = tokio::task::spawn_blocking(move || {
            remove_artist_image(&name);
            forget_override_key(&url);
        })
        .await;
        crate::artist_qt::apply_custom_image(&artist_name);
    });
}

/// "Save as…": native save dialog seeded `{name}.jpg` (the reference's default,
/// `ArtistPageView.slint:349`). Local sources first — the override file, then
/// the artwork cache — else one GET of the portrait URL.
pub fn save_artist_image_as(artist_name: String, artwork_url: String) {
    crate::spawn(async move {
        // Resolve the source BEFORE opening the dialog, so the seeded filename
        // can carry the real extension. A PNG or WebP portrait written out as
        // `.jpg` is a file that lies about itself.
        //
        // The override arm is filtered on `is_file()` for the same reason the
        // two callers in `artist_qt` are: an override whose file has since been
        // moved is `Some`, and without the filter it short-circuits the whole
        // chain — the copy then fails into a `log::warn!` and the user gets no
        // file, no error and no fallback to the cached portrait.
        let local = artist_image(&artist_name)
            .filter(|p| !p.is_empty() && std::path::Path::new(p).is_file())
            .or_else(|| {
                let cached = crate::artwork_qt::cached_path(&artwork_url);
                if cached.is_empty() {
                    None
                } else {
                    // `cached_path` hands back a file:// URL; the override map
                    // hands back a raw path. `tokio::fs::copy` takes neither
                    // form on faith, and trimming the scheme is not enough —
                    // the URL is percent-escaped.
                    Some(crate::artwork_qt::local_path(&cached))
                }
            });

        let ext = local
            .as_deref()
            .and_then(|p| std::path::Path::new(p).extension())
            .and_then(|e| e.to_str())
            .map(|e| e.to_ascii_lowercase())
            .filter(|e| matches!(e.as_str(), "jpg" | "jpeg" | "png" | "webp"))
            .unwrap_or_else(|| "jpg".to_string());

        let default_name = format!(
            "{}.{ext}",
            if artist_name.is_empty() {
                "artist"
            } else {
                &artist_name
            }
        );
        let Some(dest) = rfd::AsyncFileDialog::new()
            .set_file_name(&default_name)
            .add_filter("Image", &["jpg", "jpeg", "png", "webp"])
            .save_file()
            .await
        else {
            return;
        };
        let dest_path = dest.path().to_path_buf();

        if let Some(src) = local {
            if let Err(e) = tokio::fs::copy(&src, &dest_path).await {
                log::warn!("[qbz-qt] artist image save-as copy failed: {e}");
            }
            return;
        }
        if artwork_url.is_empty() {
            return;
        }
        let builder = match qbz_net_proxy::apply_current(reqwest::Client::builder()) {
            Ok(b) => b,
            Err(e) => {
                log::warn!("[qbz-qt] artist image save-as proxy config error: {e}");
                return;
            }
        };
        let client = match builder.timeout(std::time::Duration::from_secs(30)).build() {
            Ok(c) => c,
            Err(e) => {
                log::warn!("[qbz-qt] artist image save-as client error: {e}");
                return;
            }
        };
        match client
            .get(&artwork_url)
            .send()
            .await
            .and_then(|r| r.error_for_status())
        {
            Ok(resp) => match resp.bytes().await {
                Ok(bytes) => {
                    if let Err(e) = tokio::fs::write(&dest_path, &bytes).await {
                        log::warn!("[qbz-qt] artist image save-as write failed: {e}");
                    }
                }
                Err(e) => log::warn!("[qbz-qt] artist image save-as read failed: {e}"),
            },
            Err(e) => log::warn!("[qbz-qt] artist image save-as fetch failed: {e}"),
        }
    });
}

// ---------------------------------------------------------------------------
// Playlist custom covers (Qt-first; no Slint counterpart yet)
// ---------------------------------------------------------------------------
// Separate file on purpose: the Slint store struct round-trips only the keys
// it knows, so a `playlists` key inside custom_artwork.json would be DROPPED
// the next time the Slint app writes an album/artist override. A Qt-owned
// file cannot be clobbered by it.

#[derive(Default, Serialize, Deserialize)]
struct PlaylistStore {
    #[serde(default)]
    playlists: HashMap<String, String>,
}

fn playlist_store_path() -> Option<PathBuf> {
    Some(
        dirs::data_dir()?
            .join("qbz")
            .join("custom_playlist_covers.json"),
    )
}

fn load_playlist_store() -> PlaylistStore {
    let Some(path) = playlist_store_path() else {
        return PlaylistStore::default();
    };
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => PlaylistStore::default(),
    }
}

fn write_playlist_store(store: &PlaylistStore) {
    let Some(path) = playlist_store_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_vec_pretty(store) {
        let _ = std::fs::write(&path, json);
    }
}

/// The absolute path of the user-picked cover for `playlist_id`, if set.
pub fn playlist_cover(playlist_id: &str) -> Option<String> {
    load_playlist_store().playlists.get(playlist_id).cloned()
}

/// Refresh every live surface that owns playlist artwork after the override
/// store changes. Re-opening through the public router would navigate from the
/// Playlist Manager/sidebar into the playlist and would duplicate Back-stack
/// entries when already on the detail page, so the detail refresh is
/// explicitly conditional and navigation-free.
fn refresh_playlist_cover_surfaces(playlist_id: String) {
    crate::reload_playlist_if_showing(playlist_id.clone());
    crate::playlist_manager_qt::reload_if_loaded();
    crate::reload_sidebar_including_local();
}

// No `prefer_playlist_cover` twin of `prefer_album_cover`: the one consumer
// that would want it (`library_qt::playlist_cover_urls`) returns a Vec of
// cover refs, not a single fallback string, so it applies the same
// override-then-is_file rule itself over the mosaic. A wrapper nothing can
// call is the shape the album twin has six real call sites for.

/// Playlist header/editor "Add/Change cover": native picker, persist, then
/// refresh the detail (only when visible), manager and sidebar without
/// navigating away from the surface that opened the picker.
pub fn add_custom_playlist_cover(playlist_id: String) {
    if playlist_id.is_empty() {
        return;
    }
    crate::spawn(async move {
        let Some(file) = rfd::AsyncFileDialog::new()
            .add_filter("Images", &["png", "jpg", "jpeg", "webp"])
            .pick_file()
            .await
        else {
            return;
        };
        let path = file.path().to_string_lossy().to_string();
        let id = playlist_id.clone();
        if tokio::task::spawn_blocking(move || {
            let mut store = load_playlist_store();
            store.playlists.insert(id, path);
            write_playlist_store(&store);
        })
        .await
        .is_ok()
        {
            refresh_playlist_cover_surfaces(playlist_id);
        }
    });
}

/// "Remove cover": drop the override and refresh the live playlist surfaces
/// so they revert to the own-art/mosaic rule.
pub fn remove_custom_playlist_cover(playlist_id: String) {
    if playlist_id.is_empty() {
        return;
    }
    let id = playlist_id.clone();
    crate::spawn(async move {
        let _ = tokio::task::spawn_blocking(move || {
            let mut store = load_playlist_store();
            if store.playlists.remove(&id).is_some() {
                write_playlist_store(&store);
            }
            // A LOCAL playlist's doc also falls back to `library.db`'s
            // `custom_artwork_path` for covers an earlier build stored there.
            // Clearing only this JSON store would leave such a cover on
            // screen with the menu insisting it had been removed.
            if crate::local_playlist_qt::is_local_id(&id) {
                crate::local_playlist_qt::clear_custom_artwork_blocking(&id);
            }
        })
        .await;
        refresh_playlist_cover_surfaces(playlist_id);
    });
}

// ---------------------------------------------------------------------------
// Propagation layer: artwork-hash -> override (the "everywhere" half)
// ---------------------------------------------------------------------------
// Album overrides are keyed by album id, but most of the app never sees the
// id — cards, the NPB, the queue and mosaics carry only the art URL. The
// bridge between the two is the Qobuz cover hash: every size variant of one
// artwork shares the stem (`.../covers/tr/9l/<hash>_<size>.jpg`), so mapping
// hash -> override path lets the ARTWORK layer answer with the override no
// matter which surface asked or which size it wanted.
//
// The map lives in a Qt-side file (custom_cover_keys.json) for the same
// reason the playlist store does: the Slint store round-trips only known
// keys and would drop an extension on its next write.

#[derive(Default, Serialize, Deserialize)]
struct KeyStore {
    #[serde(default)]
    keys: HashMap<String, String>,
}

fn key_store_path() -> Option<PathBuf> {
    Some(dirs::data_dir()?.join("qbz").join("custom_cover_keys.json"))
}

fn load_key_store() -> KeyStore {
    let Some(path) = key_store_path() else {
        return KeyStore::default();
    };
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_default(),
        Err(_) => KeyStore::default(),
    }
}

fn write_key_store(store: &KeyStore) {
    let Some(path) = key_store_path() else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_vec_pretty(store) {
        let _ = std::fs::write(&path, json);
    }
}

/// The artwork hash of a Qobuz artwork URL: the filename stem before the size
/// suffix (`<hash>_600.jpg` -> `<hash>`). None for non-artwork URLs.
///
/// ARTIST PORTRAITS CARRY NO SIZE SUFFIX. Their URL is
/// `.../images/artists/covers/{small|medium|large}/<hash>.jpg` — the size is a
/// PATH segment — so the `_` rule returns None for them and both
/// `note_override_key` and `override_for_url` were silent no-ops on the artist
/// axis. The suffix-less arm is scoped to that one path prefix on purpose: a
/// global loosening would key every stray URL on its bare filename and let two
/// unrelated images collide on one override.
///
/// The `_` arm is UNCHANGED, so the rows already written into
/// `custom_cover_keys.json` under the old rule keep resolving exactly as
/// before — this widening only adds keys that could never have been written.
fn art_key(url: &str) -> Option<String> {
    let stem = url
        .rsplit('/')
        .next()
        .and_then(|f| f.rsplit_once('.').map(|(s, _)| s).or(Some(f)))
        .unwrap_or("");
    if stem.is_empty() {
        return None;
    }
    match stem.rsplit_once('_') {
        Some((hash, _size)) if !hash.is_empty() => Some(hash.to_string()),
        _ if url.contains("/images/artists/covers/") => Some(stem.to_string()),
        _ => None,
    }
}

/// Record `hash -> override path` for the album whose current art URL is
/// `artwork_url`. Called when a cover is set and on album-page load as a
/// backfill for covers set before this map existed.
pub fn note_override_key(artwork_url: &str, override_path: &str) {
    let Some(key) = art_key(artwork_url) else {
        return;
    };
    let mut store = load_key_store();
    if store.keys.get(&key).map(|s| s.as_str()) == Some(override_path) {
        return;
    }
    store.keys.insert(key, override_path.to_string());
    write_key_store(&store);
}

/// Drop the `hash -> override path` link for `artwork_url`. The counterpart of
/// [`note_override_key`], called when an override is REMOVED: without it the
/// artwork layer keeps serving the custom file to every card long after the
/// header has reverted.
fn forget_override_key(artwork_url: &str) {
    let Some(key) = art_key(artwork_url) else {
        return;
    };
    let mut store = load_key_store();
    if store.keys.remove(&key).is_some() {
        write_key_store(&store);
    }
}

/// The override path for any art URL whose hash has one ("" when none or the
/// file is gone). `artwork_qt::cached_path` consults this BEFORE the network
/// cache, so every surface renders the custom art.
pub fn override_for_url(url: &str) -> String {
    let Some(key) = art_key(url) else {
        return String::new();
    };
    match load_key_store().keys.get(&key) {
        Some(p) if std::path::Path::new(p).is_file() => p.clone(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::art_key;

    #[test]
    fn album_covers_key_on_the_stem_before_the_size_suffix() {
        assert_eq!(
            art_key("https://static.qobuz.com/images/covers/tr/9l/abc123_600.jpg").as_deref(),
            Some("abc123")
        );
        // Every size variant of one artwork resolves to the SAME key — that is
        // what makes an override apply on cards, the NPB and the queue alike.
        assert_eq!(
            art_key("https://static.qobuz.com/images/covers/tr/9l/abc123_50.jpg"),
            art_key("https://static.qobuz.com/images/covers/tr/9l/abc123_230.jpg")
        );
    }

    #[test]
    fn artist_portraits_key_on_the_bare_stem_across_every_size_segment() {
        assert_eq!(
            art_key("https://static.qobuz.com/images/artists/covers/large/784ec128.jpg").as_deref(),
            Some("784ec128")
        );
        assert_eq!(
            art_key("https://static.qobuz.com/images/artists/covers/large/784ec128.jpg"),
            art_key("https://static.qobuz.com/images/artists/covers/medium/784ec128.png")
        );
    }

    #[test]
    fn suffixless_urls_outside_the_artist_path_stay_unkeyed() {
        // The whole point of scoping the widening: an arbitrary URL must not
        // start colliding with another one on its bare filename.
        assert_eq!(art_key("https://example.com/thing/photo.jpg"), None);
        assert_eq!(
            art_key("https://static.qobuz.com/images/covers/tr/9l/abc123.jpg"),
            None
        );
        assert_eq!(art_key(""), None);
        assert_eq!(
            art_key("https://static.qobuz.com/images/artists/covers/large/"),
            None
        );
    }
}
