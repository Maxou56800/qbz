//! Desktop "now playing" notifications for track changes (frontend-agnostic, ADR-006).
//!
//! 1:1 port of the Tauri notification path (`src-tauri/src/commands_v2/
//! legacy_compat.rs::v2_show_track_notification` + the artwork helpers in
//! `helpers.rs`), lifted out of the Tauri command layer so any frontend
//! (Slint / TUI) can fire it from native Rust instead of a webview `invoke`.
//!
//!   - **Linux** → XDG notification portal via `ashpd` (goes over D-Bus). The
//!     album art is passed as `Icon::Bytes(png)` — the portal rejects huge
//!     payloads, so the cover is center-cropped to a square, downscaled to
//!     <=512px, and re-encoded PNG (<=4 MiB).
//!   - **macOS** → `notify_rust` with `image_path` (it needs a file on disk, so
//!     the cover is cached but NOT resized).
//!   - **Windows** → not implemented (parity with Tauri).
//!
//! The whole thing is fire-and-forget: failures are logged, never surfaced, so
//! a missing portal or a slow CDN never blocks playback. The HTTP download +
//! image work run on `spawn_blocking` (a tokio runtime must be present — it is,
//! the app drives one).

use std::path::PathBuf;

#[cfg(target_os = "linux")]
const PORTAL_NOTIFICATION_ID: &str = "track-now-playing";

/// The portal id of the LAST toast this process published, so the next one
/// (or a withdraw) can remove it. Ids are UNIQUE per toast on purpose
/// (`track-now-playing-<generation>`): re-publishing under one stable id is
/// an in-place update, and Plasma never re-presents an update as a banner —
/// `show-as-new` included (measured on the owner's desktop, 2026-08-31: a
/// same-id replacement with the hint produced NO popup while a fresh id
/// bannered every time). So each track change publishes a new id and removes
/// the previous one, which is the fresh-id presentation with the same
/// no-pileup lifecycle the stable id was for.
#[cfg(target_os = "linux")]
static PORTAL_NOTIFICATION_LAST: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

/// Serializes portal mutations and invalidates slow, stale artwork jobs. A
/// track-A notification can spend seconds downloading its cover while track B
/// starts (or playback stops); without this generation check A may overwrite B
/// or resurrect the notification after it was withdrawn.
#[cfg(target_os = "linux")]
static PORTAL_NOTIFICATION_GENERATION: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);
#[cfg(target_os = "linux")]
static PORTAL_NOTIFICATION_GATE: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Everything needed to render a track-change notification. The crate formats
/// the body + quality line itself so the output matches the Tauri notification
/// exactly, regardless of frontend.
#[derive(Debug, Clone, Default)]
pub struct NotificationMeta {
    pub title: String,
    pub artist: String,
    pub album: String,
    /// Bit depth (e.g. 16, 24). Drives the quality line.
    pub bit_depth: Option<u32>,
    /// Sample rate in kHz (e.g. 44.1, 96.0). Drives the quality line.
    pub sample_rate: Option<f64>,
    /// Album-art URL: http/https (downloaded + cached), `file://`, or
    /// `asset://localhost/...` (resolved to a local path). `None` = no art.
    pub art_url: Option<String>,
}

/// Format the quality line shown under the artist/album, identical to the Tauri
/// `v2_format_notification_quality`. Empty string = omit the line.
fn format_quality(bit_depth: Option<u32>, sample_rate: Option<f64>) -> String {
    match (bit_depth, sample_rate) {
        (Some(bits), Some(rate)) if bits >= 24 || rate > 48.0 => {
            let rate_str = if rate.fract() == 0.0 {
                format!("{}", rate as u32)
            } else {
                format!("{rate}")
            };
            format!("Hi-Res - {bits}-bit/{rate_str}kHz")
        }
        (Some(bits), Some(rate)) => {
            let rate_str = if rate.fract() == 0.0 {
                format!("{}", rate as u32)
            } else {
                format!("{rate}")
            };
            format!("CD Quality - {bits}-bit/{rate_str}kHz")
        }
        _ => String::new(),
    }
}

/// Build the notification body: "artist · album" then a quality line.
/// `·` (middle dot) on macOS, `•` (bullet) elsewhere — matches Tauri.
fn build_body(meta: &NotificationMeta) -> String {
    let separator = if cfg!(target_os = "macos") {
        " \u{00b7} "
    } else {
        " \u{2022} "
    };
    let mut lines = Vec::new();
    let mut line1 = Vec::new();
    if !meta.artist.is_empty() {
        line1.push(meta.artist.clone());
    }
    if !meta.album.is_empty() {
        line1.push(meta.album.clone());
    }
    if !line1.is_empty() {
        lines.push(line1.join(separator));
    }
    let quality = format_quality(meta.bit_depth, meta.sample_rate);
    if !quality.is_empty() {
        lines.push(quality);
    }
    lines.join("\n")
}

// --- artwork cache (Linux + macOS + Windows) ------------------------------------------

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn artwork_cache_dir() -> Result<PathBuf, String> {
    let dir = dirs::cache_dir()
        .ok_or_else(|| "Could not find cache directory".to_string())?
        .join("qbz")
        .join("artwork");
    std::fs::create_dir_all(&dir).map_err(|e| format!("Failed to create artwork cache dir: {e}"))?;
    Ok(dir)
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn resolve_local_artwork(url: &str) -> Option<PathBuf> {
    if let Some(path) = url.strip_prefix("file://") {
        // file:// URLs built with url::Url::from_file_path (e.g. the shared
        // disk-image cache hits handed over by playback) are percent-encoded;
        // decode so paths with spaces/non-ASCII resolve. Fall back to the raw
        // string on invalid UTF-8 escapes (a plain unencoded path).
        let decoded = urlencoding::decode(path)
            .map(|c| c.into_owned())
            .unwrap_or_else(|_| path.to_string());
        // `file:///C:/x` leaves `/C:/x` here: the empty authority's slash, in
        // front of a drive letter. PathBuf keeps it and the file never
        // resolves, so the toast silently shows no cover. Forward slashes
        // after that are fine -- Windows accepts them.
        //
        // Additive: Linux and macOS never match this shape, and their full
        // percent-decode above is left exactly as it was. It has to stay
        // full: art_url reaches here from BOTH url::Url::from_file_path
        // (which escapes spaces as %20) and fs_url::file_url (which escapes
        // only % # ?), and only the wider decode reads both.
        #[cfg(target_os = "windows")]
        {
            let b = decoded.as_bytes();
            if b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':' {
                return Some(PathBuf::from(&decoded[1..]));
            }
            // A non-empty authority is a UNC server: `file://nas/share/x`
            // names `\\nas\share\x`. Without this it stays RELATIVE and
            // resolves against the process working directory.
            if !decoded.starts_with('/') && !decoded.is_empty() {
                return Some(PathBuf::from(format!("//{decoded}")));
            }
        }
        return Some(PathBuf::from(decoded));
    }
    if let Some(path) = url.strip_prefix("asset://localhost/") {
        let decoded = urlencoding::decode(path).ok()?;
        return Some(PathBuf::from(decoded.into_owned()));
    }
    None
}

/// Shared blocking HTTP client (a fresh client per track leaks an fd → EMFILE
/// over a long session — same reasoning as the Tauri image cache).
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn http_client() -> &'static reqwest::blocking::Client {
    static CLIENT: std::sync::OnceLock<reqwest::blocking::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        qbz_net_proxy::apply_current_blocking(reqwest::blocking::Client::builder())
            .expect("apply proxy configuration")
            .pool_max_idle_per_host(2)
            .build()
            .expect("failed to build notification HTTP client")
    })
}

/// Resolve `url` to a local image file: a `file://`/`asset://` URL maps
/// straight through, an http(s) URL is downloaded and cached by md5(url).
/// `offline` = local paths + md5 cache hits only, never the HTTP download —
/// the verdict is injected by the caller so this crate stays frontend-agnostic
/// (no dependency on the app's offline-mode engine).
#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn cache_artwork(url: &str, offline: bool) -> Result<PathBuf, String> {
    use md5::{Digest, Md5};
    use std::io::Write;

    if let Some(local) = resolve_local_artwork(url) {
        if local.exists() {
            return Ok(local);
        }
    }

    let mut hasher = Md5::new();
    hasher.update(url.as_bytes());
    let hash = format!("{:x}", hasher.finalize());
    let cache_path = artwork_cache_dir()?.join(format!("{hash}.jpg"));
    if cache_path.exists() {
        return Ok(cache_path);
    }

    if offline {
        return Err("offline: artwork not cached locally".to_string());
    }

    let response = http_client()
        .get(url)
        .header("User-Agent", "Mozilla/5.0")
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .map_err(|e| format!("Failed to download artwork: {e}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "Failed to download artwork: HTTP {} (url: {})",
            response.status(),
            url.split('?').next().unwrap_or(url)
        ));
    }
    let bytes = response
        .bytes()
        .map_err(|e| format!("Failed to read artwork bytes: {e}"))?;
    let mut file =
        std::fs::File::create(&cache_path).map_err(|e| format!("Failed to create cache file: {e}"))?;
    file.write_all(&bytes)
        .map_err(|e| format!("Failed to write artwork cache: {e}"))?;
    Ok(cache_path)
}

// --- Linux: portal icon bytes -----------------------------------------------

#[cfg(target_os = "linux")]
const PORTAL_ICON_MAX_EDGE: u32 = 512;
#[cfg(target_os = "linux")]
const PORTAL_ICON_MAX_BYTES: usize = 4 * 1024 * 1024;

/// Center-crop to a square, downscale to <=512px, re-encode PNG. Mirrors the
/// Tauri `v2_prepare_notification_icon_bytes`.
///
/// Decodes by CONTENT, never by extension: the shared disk-image cache names
/// files `{md5}.img` (no real image extension), and `image::open` resolves the
/// format from the path extension only — it returned `Unsupported` for every
/// cache hit, which is exactly the common online case.
#[cfg(target_os = "linux")]
fn prepare_icon_bytes(path: &std::path::Path) -> Result<Vec<u8>, String> {
    use std::io::Cursor;

    let bytes =
        std::fs::read(path).map_err(|e| format!("Failed to read artwork {path:?}: {e}"))?;
    let source = image::load_from_memory(&bytes)
        .map_err(|e| format!("Failed to decode artwork {path:?}: {e}"))?;
    let (w, h) = (source.width(), source.height());
    let square = if w == h {
        source
    } else {
        let edge = w.min(h);
        source.crop_imm((w - edge) / 2, (h - edge) / 2, edge, edge)
    };
    let icon = if square.width() > PORTAL_ICON_MAX_EDGE {
        square.resize_exact(
            PORTAL_ICON_MAX_EDGE,
            PORTAL_ICON_MAX_EDGE,
            image::imageops::FilterType::Lanczos3,
        )
    } else {
        square
    };
    let mut buf = Cursor::new(Vec::new());
    icon.write_to(&mut buf, image::ImageFormat::Png)
        .map_err(|e| format!("Failed to encode notification PNG: {e}"))?;
    let bytes = buf.into_inner();
    if bytes.len() > PORTAL_ICON_MAX_BYTES {
        return Err(format!(
            "Notification icon too large after normalization: {} bytes (max {PORTAL_ICON_MAX_BYTES})",
            bytes.len()
        ));
    }
    Ok(bytes)
}

// --- public entry point -----------------------------------------------------

/// Show a track-change notification. Fire-and-forget: every failure is logged,
/// none propagated. Must be called from within a tokio runtime (it uses
/// `spawn_blocking` for the HTTP/image work). `offline` skips the artwork
/// HTTP download (local paths / disk-cache hits still render an icon).
pub async fn show_track_notification(meta: NotificationMeta, offline: bool) {
    let body = build_body(&meta);
    log::info!(
        "[notify] track notification: {} by {}",
        meta.title,
        meta.artist
    );

    #[cfg(target_os = "linux")]
    {
        use ashpd::desktop::notification::{Notification as PortalNotification, NotificationProxy};
        use ashpd::desktop::Icon;
        use std::sync::atomic::Ordering;

        let generation = PORTAL_NOTIFICATION_GENERATION
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);

        // A UNIQUE id per toast — see PORTAL_NOTIFICATION_LAST for why the
        // stable-id + show-as-new shape was abandoned: Plasma presents a
        // same-id replacement as a silent in-place update, hint or not. The
        // previous toast is removed right before the new publish below, so
        // the no-pileup lifecycle survives the id change.
        let notification_id = format!("{PORTAL_NOTIFICATION_ID}-{generation}");
        let mut notification = PortalNotification::new(&meta.title).body(Some(body.as_str()));

        if let Some(url) = meta.art_url.clone() {
            let prepared = tokio::task::spawn_blocking(move || -> Result<Vec<u8>, String> {
                let path = cache_artwork(&url, offline)?;
                prepare_icon_bytes(&path)
            })
            .await;
            match prepared {
                Ok(Ok(bytes)) => {
                    log::debug!("[notify] artwork prepared: {} bytes", bytes.len());
                    notification = notification.icon(Icon::Bytes(bytes));
                }
                Ok(Err(e)) => log::warn!("[notify] could not prepare artwork: {e}"),
                Err(e) => log::warn!("[notify] artwork task failed: {e}"),
            }
        }

        match NotificationProxy::new().await {
            Ok(proxy) => {
                let _guard = PORTAL_NOTIFICATION_GATE.lock().await;
                if PORTAL_NOTIFICATION_GENERATION.load(Ordering::Acquire) != generation {
                    log::debug!("[notify] stale track notification discarded");
                    return;
                }
                // Retire the previous toast FIRST: on a rapid skip its banner
                // may still be on screen, and the new publish must not stack
                // on it. The record is swapped before the awaits (a std lock
                // is never held across them); a failed add then leaves a
                // dangling id behind, whose later removal is a harmless no-op.
                let previous = PORTAL_NOTIFICATION_LAST
                    .lock()
                    .map(|mut last| last.replace(notification_id.clone()))
                    .unwrap_or(None);
                if let Some(prev) = previous {
                    if let Err(e) = proxy.remove_notification(&prev).await {
                        log::debug!("[notify] XDG portal remove_notification failed: {e}");
                    }
                }
                match proxy.add_notification(&notification_id, notification).await {
                    Ok(()) => log::debug!("[notify] XDG portal notification published"),
                    Err(e) => log::warn!("[notify] XDG portal add_notification failed: {e}"),
                }
            }
            Err(e) => log::warn!("[notify] XDG notification portal unavailable: {e}"),
        }
    }

    #[cfg(target_os = "macos")]
    {
        let _ = tokio::task::spawn_blocking(move || {
            let _ = notify_rust::set_application("com.blitzfc.qbz");
            let artwork_path = meta.art_url.as_deref().and_then(|url| match cache_artwork(url, offline) {
                Ok(path) => Some(path),
                Err(e) => {
                    log::debug!("[notify] could not cache artwork: {e}");
                    None
                }
            });
            let mut notification = notify_rust::Notification::new();
            notification.summary(&meta.title).body(&body);
            if let Some(path) = artwork_path.as_ref().and_then(|p| p.to_str()) {
                notification.image_path(path);
            }
            if let Err(e) = notification.show() {
                log::warn!("[notify] macOS notification failed: {e}");
            }
        })
        .await;
    }

    #[cfg(target_os = "windows")]
    {
        let _ = tokio::task::spawn_blocking(move || {
            let artwork_path = meta.art_url.as_deref().and_then(|url| {
                match cache_artwork(url, offline) {
                    Ok(path) => Some(path),
                    Err(e) => {
                        log::debug!("[notify] could not cache artwork: {e}");
                        None
                    }
                }
            });
            let mut notification = notify_rust::Notification::new();
            // app_id MUST match the AUMID set in main() and registered by the
            // MSI, or Windows drops the toast without a word. notify-rust's
            // Windows arm forwards summary/body/app_id/image and SILENTLY
            // ignores `actions` (src/windows.rs), so no buttons are attempted
            // here -- adding them would look implemented and do nothing.
            notification
                .summary(&meta.title)
                .body(&body)
                .app_id("com.blitzfc.qbz");
            if let Some(path) = artwork_path.as_ref().and_then(|p| p.to_str()) {
                notification.image_path(path);
            }
            if let Err(e) = notification.show() {
                log::warn!("[notify] Windows toast failed: {e}");
            }
        })
        .await;
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        let _ = body;
        let _ = offline;
        log::info!("[notify] desktop notifications not implemented on this platform");
    }
}

/// Withdraw the active Linux portal notification, if any. This also
/// invalidates an in-flight artwork preparation so it cannot publish after a
/// stop or process shutdown. Other platforms currently have no replaceable
/// notification handle, so this is a deliberate no-op there.
pub async fn withdraw_track_notification() {
    #[cfg(target_os = "linux")]
    {
        use ashpd::desktop::notification::NotificationProxy;
        use std::sync::atomic::Ordering;

        let generation = PORTAL_NOTIFICATION_GENERATION
            .fetch_add(1, Ordering::AcqRel)
            .wrapping_add(1);
        let proxy = match NotificationProxy::new().await {
            Ok(proxy) => proxy,
            Err(e) => {
                log::debug!("[notify] XDG notification portal unavailable during withdrawal: {e}");
                return;
            }
        };
        let _guard = PORTAL_NOTIFICATION_GATE.lock().await;
        if PORTAL_NOTIFICATION_GENERATION.load(Ordering::Acquire) != generation {
            return;
        }
        // The ids are per-toast now (see PORTAL_NOTIFICATION_LAST) — take and
        // remove whichever one this process last published. Nothing published
        // = nothing to withdraw.
        let target = PORTAL_NOTIFICATION_LAST
            .lock()
            .map(|mut last| last.take())
            .unwrap_or(None);
        let Some(target) = target else {
            return;
        };
        if let Err(e) = proxy.remove_notification(&target).await {
            log::debug!("[notify] XDG portal remove_notification failed: {e}");
        }
    }
}
