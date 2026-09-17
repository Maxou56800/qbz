//! The desktop wallpaper as the app-wide background (2026-09-14).
//!
//! Two `app_background` modes ride on it: "wallpaper" (3) makes the window
//! read as translucent to the desktop — ALWAYS the part of the wallpaper
//! that lies under the window, wherever it is moved, lightly blurred under
//! the half-alpha chrome (shell/WallpaperField.qml). The window's place on
//! the screen comes from Qt where the platform tells it (X11, macOS,
//! Windows) and from the compositor on KDE Plasma Wayland
//! (wallpaper_wayland_qt.rs, `org_kde_plasma_window_management`); where
//! neither can say, a centred crop stands in. "wallpaper-blurred" (4) is
//! Blurred art with the wallpaper in the cover's place: the same
//! ImmersiveAtmosphere, the same drift while the transport plays
//! (atmosphere_qt::for_cover_blocking builds its bitmap).
//!
//! The image is the system wallpaper, resolved per desktop — KDE Plasma's
//! `plasma-org.kde.plasma.desktop-appletsrc` (the containment on screen 0
//! wins; a wallpaper PACKAGE resolves to its largest `contents/images/*`
//! bitmap, a slideshow folder to its first image), GNOME through `gsettings`
//! (the dark variant under a dark colour scheme), Windows' current
//! `TranscodedWallpaper`, macOS through System Events — or an image the user
//! chose (`app_background_image`), which outranks the desktop. `refresh()`
//! is cheap to call often: the resolved path and its mtime are memoised, and
//! nothing is republished while they hold.

use cxx_qt_lib::QString;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

pub const MODE_WALLPAPER: i32 = 3;

static LAST: Mutex<Option<(PathBuf, Option<SystemTime>)>> = Mutex::new(None);

const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "webp", "bmp", "avif", "jxl"];

/// Does this `app_background` mode paint the wallpaper?
pub fn mode_uses_wallpaper(mode: i32) -> bool {
    mode >= MODE_WALLPAPER
}

/// Start following the window's place on a Plasma Wayland desktop
/// (wallpaper_wayland_qt.rs). Idempotent; nothing to start elsewhere.
pub fn track_window_position() {
    #[cfg(target_os = "linux")]
    crate::wallpaper_wayland_qt::start();
}

/// Resolve the wallpaper (the user's own image first) and publish its URL and
/// its atmosphere bitmap on the shell bridge. No-op while the mode does not
/// paint it, and quiet while the same file (same mtime) is already published.
pub fn refresh() {
    if !mode_uses_wallpaper(crate::settings_qt::app_background_mode()) {
        return;
    }
    crate::spawn(async {
        let resolved = tokio::task::spawn_blocking(|| {
            let custom = crate::settings_qt::pref_str("app_background_image", "");
            let path = if custom.trim().is_empty() {
                resolve_system()
            } else {
                Some(PathBuf::from(custom.trim()))
            };
            let path = path.filter(|p| p.is_file())?;
            let mtime = std::fs::metadata(&path)
                .ok()
                .and_then(|m| m.modified().ok());
            {
                let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
                if last
                    .as_ref()
                    .is_some_and(|(p, t)| *p == path && *t == mtime)
                {
                    return None;
                }
                *last = Some((path.clone(), mtime));
            }
            let atmosphere = crate::atmosphere_qt::for_cover_blocking(&path.to_string_lossy())
                .unwrap_or_default();
            Some((file_url(&path), atmosphere))
        })
        .await
        .ok()
        .flatten();
        if let Some((url, atmosphere)) = resolved {
            log::info!("[qbz-qt] wallpaper: {url}");
            crate::shell_bridge::ui(move |mut b| {
                b.as_mut().set_wallpaper_url(QString::from(url.as_str()));
                b.as_mut()
                    .set_wallpaper_atmosphere_url(QString::from(atmosphere.as_str()));
            });
        }
    });
}

/// The desktop's current wallpaper, if this desktop tells us.
pub fn resolve_system() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    return windows_wallpaper();
    #[cfg(target_os = "macos")]
    return macos_wallpaper();
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    return kde_wallpaper().or_else(gnome_wallpaper);
}

fn file_url(path: &Path) -> String {
    let text = path.to_string_lossy();
    if text.starts_with('/') {
        format!("file://{text}")
    } else {
        // Windows: `C:\a\b.png` -> `file:///C:/a/b.png`.
        format!("file:///{}", text.replace('\\', "/"))
    }
}

/// `%20` and friends back to bytes (the desktops store wallpaper paths as
/// URLs). Anything malformed passes through untouched.
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = &s[i + 1..i + 3];
            if let Ok(v) = u8::from_str_radix(hex, 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A wallpaper setting's value (`file:///…` or a bare path) to a path.
fn path_from_setting(value: &str) -> PathBuf {
    let v = value.trim().trim_matches('\'').trim_matches('"');
    let v = v.strip_prefix("file://").unwrap_or(v);
    PathBuf::from(percent_decode(v))
}

fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| IMAGE_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

/// A file stays a file; a directory is either a KDE wallpaper PACKAGE
/// (`contents/images/<WxH>.<ext>`, the largest wins) or a slideshow folder
/// (its first image, sorted).
fn resolve_image_source(path: PathBuf) -> Option<PathBuf> {
    if path.is_file() {
        return Some(path);
    }
    if !path.is_dir() {
        return None;
    }
    let package = path.join("contents").join("images");
    let dir = if package.is_dir() { package } else { path };
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && is_image(p))
        .collect();
    if files.is_empty() {
        return None;
    }
    files.sort();
    let pixels = |p: &Path| -> u64 {
        p.file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| {
                let (w, h) = s.split_once('x')?;
                Some(w.parse::<u64>().ok()? * h.parse::<u64>().ok()?)
            })
            .unwrap_or(0)
    };
    // The FIRST of equals wins (a slideshow folder has no sizes in its names),
    // which `max_by_key` would not give: it returns the last maximum.
    let mut best = files[0].clone();
    let mut best_pixels = pixels(&best);
    for file in &files[1..] {
        let px = pixels(file);
        if px > best_pixels {
            best = file.clone();
            best_pixels = px;
        }
    }
    Some(best)
}

fn section_ids(section: &str) -> Vec<&str> {
    section
        .trim_matches(|c| c == '[' || c == ']')
        .split("][")
        .collect()
}

/// The image Plasma paints on screen 0 (else the lowest screen, else any):
/// `[Containments][N][Wallpaper][org.kde.image][General] Image=`, for the
/// containments whose wallpaper plugin is the image one.
fn kde_image_from_config(text: &str) -> Option<String> {
    let mut section = String::new();
    let mut screen_of: HashMap<String, i32> = HashMap::new();
    let mut plugin_of: HashMap<String, String> = HashMap::new();
    let mut image_of: HashMap<String, String> = HashMap::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with('[') {
            section = line.to_string();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let ids = section_ids(&section);
        if ids.len() == 2 && ids[0] == "Containments" {
            match key {
                "lastScreen" => {
                    if let Ok(n) = value.trim().parse::<i32>() {
                        screen_of.insert(ids[1].to_string(), n);
                    }
                }
                "wallpaperplugin" => {
                    plugin_of.insert(ids[1].to_string(), value.trim().to_string());
                }
                _ => {}
            }
        } else if ids.len() == 5
            && ids[0] == "Containments"
            && ids[2] == "Wallpaper"
            && ids[3] == "org.kde.image"
            && ids[4] == "General"
            && key == "Image"
            && !value.trim().is_empty()
        {
            image_of.insert(ids[1].to_string(), value.trim().to_string());
        }
    }
    let mut best: Option<(i32, String)> = None;
    for (id, image) in image_of {
        if plugin_of.get(&id).is_some_and(|p| p != "org.kde.image") {
            continue;
        }
        let screen = screen_of.get(&id).copied().unwrap_or(i32::MAX);
        let rank = if screen < 0 { i32::MAX } else { screen };
        if best.as_ref().is_none_or(|(r, _)| rank < *r) {
            best = Some((rank, image));
        }
    }
    best.map(|(_, image)| image)
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn kde_wallpaper() -> Option<PathBuf> {
    let config = dirs::config_dir()?.join("plasma-org.kde.plasma.desktop-appletsrc");
    let text = std::fs::read_to_string(config).ok()?;
    let value = kde_image_from_config(&text)?;
    resolve_image_source(path_from_setting(&value))
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn gsettings(schema: &str, key: &str) -> Option<String> {
    let out = std::process::Command::new("gsettings")
        .args(["get", schema, key])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[cfg(not(any(target_os = "windows", target_os = "macos")))]
fn gnome_wallpaper() -> Option<PathBuf> {
    let dark = gsettings("org.gnome.desktop.interface", "color-scheme")
        .is_some_and(|v| v.contains("prefer-dark"));
    let key = if dark {
        "picture-uri-dark"
    } else {
        "picture-uri"
    };
    let value = gsettings("org.gnome.desktop.background", key)
        .filter(|v| !v.trim_matches('\'').is_empty())
        .or_else(|| gsettings("org.gnome.desktop.background", "picture-uri"))?;
    resolve_image_source(path_from_setting(&value))
}

#[cfg(target_os = "windows")]
fn windows_wallpaper() -> Option<PathBuf> {
    let appdata = std::env::var_os("APPDATA")?;
    let transcoded = PathBuf::from(appdata)
        .join("Microsoft")
        .join("Windows")
        .join("Themes")
        .join("TranscodedWallpaper");
    if transcoded.is_file() {
        return Some(transcoded);
    }
    let out = std::process::Command::new("reg")
        .args(["query", r"HKCU\Control Panel\Desktop", "/v", "WallPaper"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().find(|l| l.contains("WallPaper"))?;
    let value = line.split("REG_SZ").nth(1)?.trim();
    resolve_image_source(PathBuf::from(value))
}

#[cfg(target_os = "macos")]
fn macos_wallpaper() -> Option<PathBuf> {
    let out = std::process::Command::new("osascript")
        .args([
            "-e",
            "tell application \"System Events\" to get picture of current desktop",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if value.is_empty() {
        return None;
    }
    resolve_image_source(PathBuf::from(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    const APPLETSRC: &str = r#"
[Containments][1]
activityId=abc
lastScreen=1
plugin=org.kde.plasma.folder
wallpaperplugin=org.kde.image

[Containments][1][Wallpaper][org.kde.image][General]
Image=file:///home/me/Pictures/Second%20Screen.png

[Containments][2]
activityId=abc
lastScreen=0
plugin=org.kde.plasma.folder
wallpaperplugin=org.kde.image

[Containments][2][Wallpaper][org.kde.color][General]
Color=0,0,0

[Containments][2][Wallpaper][org.kde.image][General]
Image=file:///usr/share/wallpapers/MilkyWay/

[Containments][3]
lastScreen=0
wallpaperplugin=org.kde.color

[Containments][3][Wallpaper][org.kde.image][General]
Image=file:///home/me/stale.png
"#;

    #[test]
    fn plasma_picks_the_image_containment_on_screen_zero() {
        assert_eq!(
            kde_image_from_config(APPLETSRC).as_deref(),
            Some("file:///usr/share/wallpapers/MilkyWay/")
        );
        // Without screen 0 the lowest screen wins; a colour containment never does.
        let only_second = APPLETSRC.replace("lastScreen=0", "lastScreen=5");
        assert_eq!(
            kde_image_from_config(&only_second).as_deref(),
            Some("file:///home/me/Pictures/Second%20Screen.png")
        );
        assert!(kde_image_from_config("[Containments][9]\nlastScreen=0\n").is_none());
    }

    #[test]
    fn settings_values_become_decoded_paths() {
        assert_eq!(
            path_from_setting("file:///home/me/Pictures/Second%20Screen.png"),
            PathBuf::from("/home/me/Pictures/Second Screen.png")
        );
        assert_eq!(
            path_from_setting("'file:///usr/share/backgrounds/a.jpg'"),
            PathBuf::from("/usr/share/backgrounds/a.jpg")
        );
        assert_eq!(
            path_from_setting("/plain/path.png"),
            PathBuf::from("/plain/path.png")
        );
        assert_eq!(percent_decode("100%"), "100%");
    }

    #[test]
    fn a_package_resolves_to_its_largest_bitmap_and_a_folder_to_its_first_image() {
        let temp = tempfile::tempdir().unwrap();
        let package = temp.path().join("MilkyWay");
        let images = package.join("contents").join("images");
        std::fs::create_dir_all(&images).unwrap();
        for name in ["1080x1920.png", "5120x2880.png", "notes.txt"] {
            std::fs::write(images.join(name), b"x").unwrap();
        }
        assert_eq!(
            resolve_image_source(package.clone()),
            Some(images.join("5120x2880.png"))
        );
        let slides = temp.path().join("slides");
        std::fs::create_dir_all(&slides).unwrap();
        for name in ["b.jpg", "a.png", "readme.md"] {
            std::fs::write(slides.join(name), b"x").unwrap();
        }
        assert_eq!(
            resolve_image_source(slides.clone()),
            Some(slides.join("a.png"))
        );
        let file = temp.path().join("one.webp");
        std::fs::write(&file, b"x").unwrap();
        assert_eq!(resolve_image_source(file.clone()), Some(file));
        assert!(resolve_image_source(temp.path().join("missing")).is_none());
    }
}
