use crate::{Asset, Result};
use base64::Engine;
use std::{
    io::Read,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

// Public verification key from the shipping Tauri updater. Not an API secret.
const PUBLIC_KEY: &str = "RWSGlbvrbc5/P3/4zoXZfBd+pee5Kw7h5/gUIE7B9GiL47dvUSNpNGLm";
const MAX_DOWNLOAD: u64 = 2 * 1024 * 1024 * 1024;

pub enum InstallOutcome {
    Installed(PathBuf),
    Prepared(PathBuf),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Installation {
    AppImage(PathBuf),
    MacBundle(PathBuf),
    WindowsMsi(PathBuf),
    Flatpak,
    Snap,
    Nix,
    Managed,
}
impl Installation {
    pub fn detect() -> Self {
        if std::env::var_os("FLATPAK_ID").is_some() || Path::new("/.flatpak-info").exists() {
            return Self::Flatpak;
        }
        if std::env::var_os("SNAP").is_some() {
            return Self::Snap;
        }
        let exe = std::env::current_exe().unwrap_or_default();
        #[cfg(windows)]
        if crate::windows::is_msi_install(&exe) {
            return Self::WindowsMsi(exe);
        }
        if exe.starts_with("/nix/store") {
            return Self::Nix;
        }
        if cfg!(target_os = "linux") {
            if let Some(path) = std::env::var_os("APPIMAGE").map(PathBuf::from) {
                // The runtime supplies the original image, outside its mount.
                if path.is_absolute() && path.is_file() {
                    return Self::AppImage(path);
                }
            }
        }
        if cfg!(target_os = "macos") {
            if let Some(bundle) = exe
                .ancestors()
                .find(|p| p.extension().is_some_and(|x| x == "app"))
            {
                // Homebrew owns bundles in its cellar; do not replace them.
                if !bundle.starts_with("/Volumes")
                    && !bundle.to_string_lossy().contains("/Caskroom/")
                    && mac_bundle_uses_upstream_signature(bundle)
                {
                    return Self::MacBundle(bundle.into());
                }
            }
        }
        Self::Managed
    }
    pub fn label(&self) -> &'static str {
        match self {
            Self::AppImage(_) => "AppImage",
            Self::MacBundle(_) => "macOS",
            Self::WindowsMsi(_) => "Windows MSI",
            Self::Flatpak => "Flatpak",
            Self::Snap => "Snap",
            Self::Nix => "Nix",
            Self::Managed => "System / manual",
        }
    }
    pub fn platform(&self) -> Option<String> {
        if !matches!(std::env::consts::ARCH, "x86_64" | "aarch64") {
            return None;
        }
        let os = match self {
            Self::AppImage(_) => "linux",
            Self::MacBundle(_) => "darwin",
            Self::WindowsMsi(_) if std::env::consts::ARCH == "x86_64" => "windows",
            _ => return None,
        };
        Some(format!("{os}-{}", std::env::consts::ARCH))
    }
    fn destination(&self) -> Result<&Path> {
        match self {
            Self::AppImage(p) | Self::MacBundle(p) | Self::WindowsMsi(p) => Ok(p),
            _ => Err("This installation is managed outside QBZ".into()),
        }
    }
}

fn mac_bundle_uses_upstream_signature(bundle: &Path) -> bool {
    #[cfg(target_os = "macos")]
    {
        // Community DMGs/Homebrew use a Developer ID and notarization. The
        // upstream manifest contains an ad-hoc bundle: never downgrade that
        // trust channel merely because both bundles are named QBZ.app.
        let Ok(output) = std::process::Command::new("/usr/bin/codesign")
            .args(["--display", "--verbose=2"])
            .arg(bundle)
            .output()
        else {
            return false;
        };
        output.status.success()
            && upstream_signature_description(&String::from_utf8_lossy(&output.stderr))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = bundle;
        false
    }
}

#[cfg(any(target_os = "macos", test))]
fn upstream_signature_description(description: &str) -> bool {
    description.lines().any(|l| l.trim() == "Signature=adhoc")
        && !description
            .lines()
            .any(|l| l.starts_with("TeamIdentifier=") && l != "TeamIdentifier=not set")
}

fn cancelled(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        Err("Update cancelled".into())
    } else {
        Ok(())
    }
}

fn signature(encoded: &str) -> Result<minisign_verify::Signature> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded.trim())
        .map_err(|e| e.to_string())?;
    let text = std::str::from_utf8(&bytes).map_err(|e| e.to_string())?;
    minisign_verify::Signature::decode(text).map_err(|e| e.to_string())
}

pub fn verify(file: &Path, encoded: &str) -> Result<()> {
    verify_with_key(file, encoded, PUBLIC_KEY)
}

fn verify_with_key(file: &Path, encoded: &str, public_key: &str) -> Result<()> {
    let key = minisign_verify::PublicKey::from_base64(public_key).map_err(|e| e.to_string())?;
    let sig = signature(encoded)?;
    let mut verifier = key.verify_stream(&sig).map_err(|e| e.to_string())?;
    let mut input = std::fs::File::open(file).map_err(|e| e.to_string())?;
    let mut buffer = [0u8; 65536];
    loop {
        let n = input.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        verifier.update(&buffer[..n]);
    }
    verifier
        .finalize()
        .map_err(|e| format!("Update signature verification failed: {e}"))
}

/// Downloads onto the installation filesystem, verifies before touching the
/// live application, and keeps the old application as a rollback copy.
/// The caller serializes installs and disables exit during final replacement.
pub async fn install(
    installation: Installation,
    asset: Asset,
    cancel: &AtomicBool,
    progress: impl Fn(&str, u64, Option<u64>) + Send + Sync,
) -> Result<InstallOutcome> {
    let destination = installation.destination()?.to_path_buf();
    let parent = destination
        .parent()
        .ok_or("Installation has no parent directory")?;
    // Advisory OS lock survives an async hop and is released after a crash.
    // Keep the lock file: removing it would let another process lock a new inode.
    let install_lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(parent.join(".qbz-updater.lock"))
        .map_err(|e| e.to_string())?;
    install_lock
        .try_lock()
        .map_err(|e| format!("Another update is already running: {e}"))?;
    // Do not follow a launcher symlink and unexpectedly replace its target.
    let original = std::fs::symlink_metadata(&destination).map_err(|e| e.to_string())?;
    if original.file_type().is_symlink() {
        return Err(
            "This installation is a symlink; update it using its installation manager".into(),
        );
    }
    let stage = tempfile::Builder::new()
        .prefix(".qbz-update-")
        .tempdir_in(parent)
        .map_err(|e| format!("Cannot write to the installation directory: {e}"))?;
    let payload = stage.path().join("download");
    let mut output = tokio::fs::File::create(&payload)
        .await
        .map_err(|e| e.to_string())?;
    cancelled(cancel)?;
    let mut response = crate::client(Duration::from_secs(30 * 60))?
        .get(&asset.url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| e.to_string())?;
    let total = response.content_length();
    if total.is_some_and(|v| v > MAX_DOWNLOAD) {
        return Err("Update exceeds size limit".into());
    }
    let mut downloaded = 0;
    let mut last = Instant::now();
    progress("downloading", 0, total);
    loop {
        cancelled(cancel)?;
        // Bounded inactivity, separate from the full download deadline.
        let chunk = tokio::time::timeout(Duration::from_secs(30), response.chunk())
            .await
            .map_err(|_| "Update download timed out")?
            .map_err(|e| e.to_string())?;
        let Some(chunk) = chunk else { break };
        downloaded += chunk.len() as u64;
        if downloaded > MAX_DOWNLOAD {
            return Err("Update exceeds size limit".into());
        }
        tokio::io::AsyncWriteExt::write_all(&mut output, &chunk)
            .await
            .map_err(|e| e.to_string())?;
        if last.elapsed() >= Duration::from_millis(200) {
            progress("downloading", downloaded, total);
            last = Instant::now();
        }
    }
    output.sync_all().await.map_err(|e| e.to_string())?;
    drop(output);
    cancelled(cancel)?;
    progress("verifying", downloaded, total);
    let signature = asset.signature;
    let verify_path = payload.clone();
    tokio::task::spawn_blocking(move || verify(&verify_path, &signature))
        .await
        .map_err(|e| e.to_string())??;
    cancelled(cancel)?;
    progress("installing", downloaded, total);
    tokio::task::spawn_blocking(move || {
        let _lock = install_lock;
        let current = std::fs::symlink_metadata(&destination).map_err(|e| e.to_string())?;
        if current.file_type().is_symlink()
            || current.len() != original.len()
            || current.modified().ok() != original.modified().ok()
        {
            return Err(
                "The installation changed while downloading; check for updates again".into(),
            );
        }
        #[cfg(windows)]
        if let Installation::WindowsMsi(exe) = &installation {
            return crate::windows::stage_msi(exe, stage, &payload).map(InstallOutcome::Prepared);
        }
        apply(&installation, stage, &payload).map(InstallOutcome::Installed)
    })
    .await
    .map_err(|e| e.to_string())?
}

fn apply(installation: &Installation, stage: tempfile::TempDir, payload: &Path) -> Result<PathBuf> {
    let destination = installation.destination()?;
    let backup = stage.path().join("previous");
    match installation {
        Installation::AppImage(_) => {
            // Signatures authenticate bytes; format validation prevents a
            // mismatched official asset from replacing a working installation.
            let mut header = [0u8; 11];
            std::fs::File::open(payload)
                .and_then(|mut f| f.read_exact(&mut header))
                .map_err(|e| e.to_string())?;
            if &header[..4] != b"\x7fELF" || &header[8..11] != b"AI\x02" {
                return Err("The signed file is not an AppImage".into());
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(payload, std::fs::Permissions::from_mode(0o755))
                    .map_err(|e| e.to_string())?;
            }
            // Hard-link backup keeps the live path present until atomic rename.
            std::fs::hard_link(destination, &backup).map_err(|e| e.to_string())?;
            std::fs::rename(payload, destination).map_err(|e| e.to_string())?;
        }
        Installation::MacBundle(_) => {
            #[cfg(target_os = "macos")]
            if !mac_bundle_uses_upstream_signature(destination) {
                return Err(
                    "This macOS app must be updated through its original signing channel".into(),
                );
            }
            let unpacked = stage.path().join("unpacked");
            unpack_bundle(payload, &unpacked)?;
            let bundle = unpacked.join("QBZ.app");
            #[cfg(target_os = "macos")]
            {
                let status = std::process::Command::new("/usr/bin/codesign")
                    .args(["--verify", "--deep", "--strict"])
                    .arg(&bundle)
                    .status()
                    .map_err(|e| e.to_string())?;
                if !status.success() {
                    return Err("The updated application failed code-signature validation".into());
                }
            }
            std::fs::rename(destination, &backup).map_err(|e| e.to_string())?;
            if let Err(error) = std::fs::rename(&bundle, destination) {
                if let Err(rollback) = std::fs::rename(&backup, destination) {
                    let retained = stage.keep();
                    return Err(format!("Install failed: {error}; restore failed: {rollback}. Original retained at {}", retained.join("previous").display()));
                }
                return Err(error.to_string());
            }
        }
        _ => return Err("This installation is managed outside QBZ".into()),
    }
    // Retain ONE old application per explicit update; never delete files a
    // still-running process may need. No cleanup of arbitrary sibling folders.
    let retained = stage.keep();
    Ok(retained.join("previous"))
}

fn unpack_bundle(payload: &Path, destination: &Path) -> Result<()> {
    std::fs::create_dir_all(destination).map_err(|e| e.to_string())?;
    let file = std::fs::File::open(payload).map_err(|e| e.to_string())?;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(file));
    let mut size = 0u64;
    for entry in archive.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path().map_err(|e| e.to_string())?.into_owned();
        if !path.starts_with("QBZ.app")
            || path.components().any(|c| {
                matches!(
                    c,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )
            })
        {
            return Err("Update archive contains an invalid path".into());
        }
        if let Some(target) = entry.link_name().map_err(|e| e.to_string())? {
            // Framework bundles contain relative symlinks. Permit those only
            // while their lexical target remains inside the staged QBZ.app.
            let base = if entry.header().entry_type().is_hard_link() {
                Path::new("")
            } else {
                path.parent().unwrap_or(Path::new(""))
            };
            let mut resolved = PathBuf::new();
            for component in base.join(target.as_ref()).components() {
                match component {
                    std::path::Component::Normal(name) => resolved.push(name),
                    std::path::Component::CurDir => {}
                    std::path::Component::ParentDir => {
                        if !resolved.pop() {
                            return Err("Update archive link escapes its bundle".into());
                        }
                    }
                    _ => return Err("Update archive contains an absolute link".into()),
                }
            }
            if !resolved.starts_with("QBZ.app") {
                return Err("Update archive link escapes its bundle".into());
            }
        }
        size = size
            .checked_add(entry.size())
            .ok_or("Update archive size overflow")?;
        if size > 4 * MAX_DOWNLOAD {
            return Err("Update archive exceeds size limit".into());
        }
        if !entry.unpack_in(destination).map_err(|e| e.to_string())? {
            return Err("Update archive escaped its staging directory".into());
        }
    }
    if !destination.join("QBZ.app/Contents/MacOS/qbz").is_file()
        || !destination.join("QBZ.app/Contents/Info.plist").is_file()
    {
        return Err("Update archive does not contain a QBZ application".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn notarized_macos_channel_is_never_replaced_with_an_ad_hoc_bundle() {
        assert!(upstream_signature_description(
            "Signature=adhoc\nTeamIdentifier=not set\n"
        ));
        assert!(!upstream_signature_description(
            "Authority=Developer ID Application: Publisher\nTeamIdentifier=ABC123\n"
        ));
        assert!(!upstream_signature_description(
            "Signature=adhoc\nTeamIdentifier=ABC123\n"
        ));
        assert!(!upstream_signature_description(""));
    }
    #[test]
    fn public_key_and_bad_signature_fail_closed() {
        assert!(minisign_verify::PublicKey::from_base64(PUBLIC_KEY).is_ok());
        let f = tempfile::NamedTempFile::new().unwrap();
        assert!(verify(f.path(), "not a signature").is_err());
    }
    #[test]
    fn tauri_envelope_verifies_bytes_and_rejects_tampering_or_wrong_key() {
        // Public test vector from minisign-verify (MIT); same prehashed
        // signature + outer base64 envelope used by the QBZ release signer.
        let key = "RWQf6LRCGA9i53mlYecO4IzT51TGPpvWucNSCh1CBM0QTaLn73Y7GFO3";
        let text = "untrusted comment: signature from minisign secret key\nRUQf6LRCGA9i559r3g7V1qNyJDApGip8MfqcadIgT9CuhV3EMhHoN1mGTkUidF/z7SrlQgXdy8ofjb7bNJJylDOocrCo8KLzZwo=\ntrusted comment: timestamp:1556193335\tfile:test\ny/rUw2y8/hOUYjZU71eHp/Wo1KZ40fGy2VJEDl34XMJM+TX48Ss/17u3IvIfbVR1FkZZSNCisQbuQY+bHwhEBg==";
        let signature = base64::engine::general_purpose::STANDARD.encode(text);
        let f = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(f.path(), b"test").unwrap();
        assert!(verify_with_key(f.path(), &signature, key).is_ok());
        assert!(verify(f.path(), &signature).is_err());
        std::fs::write(f.path(), b"tampered").unwrap();
        assert!(verify_with_key(f.path(), &signature, key).is_err());
    }
    #[test]
    fn mac_archive_rejects_symlink_outside_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let payload = dir.path().join("update.tar.gz");
        let gz = flate2::write::GzEncoder::new(
            std::fs::File::create(&payload).unwrap(),
            flate2::Compression::fast(),
        );
        let mut tar = tar::Builder::new(gz);
        let mut h = tar::Header::new_gnu();
        h.set_entry_type(tar::EntryType::Symlink);
        h.set_size(0);
        h.set_mode(0o777);
        tar.append_link(&mut h, "QBZ.app/escape", "../../outside")
            .unwrap();
        tar.into_inner().unwrap().finish().unwrap();
        assert!(unpack_bundle(&payload, &dir.path().join("unpacked")).is_err());
        assert!(!dir.path().join("unpacked/QBZ.app/escape").exists());
    }

    #[cfg(not(target_os = "macos"))]
    #[test]
    fn mac_bundle_layout_replaces_whole_bundle_and_retains_previous_files() {
        // Exercise archive/filesystem behavior independently of macOS's native
        // codesign gate; that gate still requires a real macOS smoke test.
        let dir = tempfile::tempdir().unwrap();
        let destination = dir.path().join("QBZ.app");
        std::fs::create_dir_all(destination.join("Contents/MacOS")).unwrap();
        std::fs::write(destination.join("Contents/MacOS/qbz"), b"old executable").unwrap();
        std::fs::write(destination.join("old-only"), b"old resource").unwrap();
        let stage = tempfile::tempdir_in(dir.path()).unwrap();
        let payload = stage.path().join("download");
        let gz = flate2::write::GzEncoder::new(
            std::fs::File::create(&payload).unwrap(),
            flate2::Compression::fast(),
        );
        let mut tar = tar::Builder::new(gz);
        for (path, bytes) in [
            ("QBZ.app/Contents/MacOS/qbz", b"new executable".as_slice()),
            ("QBZ.app/Contents/Info.plist", b"bundle metadata".as_slice()),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            tar.append_data(&mut header, path, bytes).unwrap();
        }
        tar.into_inner().unwrap().finish().unwrap();
        let backup = apply(
            &Installation::MacBundle(destination.clone()),
            stage,
            &payload,
        )
        .unwrap();
        assert_eq!(
            std::fs::read(destination.join("Contents/MacOS/qbz")).unwrap(),
            b"new executable"
        );
        assert!(!destination.join("old-only").exists());
        assert_eq!(
            std::fs::read(backup.join("Contents/MacOS/qbz")).unwrap(),
            b"old executable"
        );
        assert_eq!(
            std::fs::read(backup.join("old-only")).unwrap(),
            b"old resource"
        );
    }
    #[test]
    fn package_managers_never_offer_binary_replacement() {
        for install in [
            Installation::Flatpak,
            Installation::Snap,
            Installation::Nix,
            Installation::Managed,
        ] {
            assert!(install.platform().is_none());
            assert!(install.destination().is_err());
        }
    }
    #[test]
    fn invalid_payload_leaves_installation_untouched() {
        let d = tempfile::tempdir().unwrap();
        let dest = d.path().join("QBZ.AppImage");
        std::fs::write(&dest, b"old application").unwrap();
        let stage = tempfile::tempdir_in(d.path()).unwrap();
        let payload = stage.path().join("download");
        std::fs::write(&payload, b"wrong payload bytes").unwrap();
        assert!(apply(&Installation::AppImage(dest.clone()), stage, &payload).is_err());
        assert_eq!(std::fs::read(dest).unwrap(), b"old application");
    }
    #[test]
    fn replacement_retains_previous_application() {
        let d = tempfile::tempdir().unwrap();
        let dest = d.path().join("QBZ.AppImage");
        std::fs::write(&dest, b"old application").unwrap();
        let stage = tempfile::tempdir_in(d.path()).unwrap();
        let payload = stage.path().join("download");
        let image = b"\x7fELF\0\0\0\0AI\x02new application";
        std::fs::write(&payload, image).unwrap();
        let backup = apply(&Installation::AppImage(dest.clone()), stage, &payload).unwrap();
        assert_eq!(std::fs::read(dest).unwrap(), image);
        assert_eq!(std::fs::read(backup).unwrap(), b"old application");
    }
}
