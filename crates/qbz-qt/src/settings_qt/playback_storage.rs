//! Playback L2/spool location. Selection is validated now and used next launch.
use std::{path::{Path, PathBuf}, sync::{LazyLock, Mutex}};
use serde::Serialize;

#[derive(Default)]
struct Draft { candidate: Option<String>, error: String, generation: u64 }
static DRAFT: LazyLock<Mutex<Draft>> = LazyLock::new(|| Mutex::new(Draft::default()));

#[derive(Default, Serialize)]
pub struct Snapshot {
    pub candidate: String,
    pub configured: String,
    pub active: String,
    pub error: String,
    pub sandbox: String,
    pub command: String,
}

fn quote(value: &str) -> String { format!("'{}'", value.replace('\'', "'\\''")) }

fn access_command(path: &str, flatpak: Option<&str>, snap: Option<&str>) -> (String, String) {
    if let Some(app) = flatpak {
        let command = if Path::new(path).is_absolute() {
            format!("flatpak override --user --filesystem={} {}", quote(&format!("{path}:rw")), quote(app))
        } else { String::new() };
        return ("Flatpak".into(), command);
    }
    if let Some(app) = snap {
        let external = ["/mnt", "/media", "/run/media"].iter().any(|root| Path::new(path).starts_with(root));
        return ("Snap".into(), if external {
            format!("sudo snap connect {}", quote(&format!("{app}:removable-media")))
        } else { String::new() });
    }
    (String::new(), String::new())
}

pub(super) fn snapshot() -> Snapshot {
    let configured = super::audio_settings().playback_cache.disk_directory.unwrap_or_default();
    let draft = DRAFT.lock().unwrap_or_else(|e| e.into_inner());
    let candidate = draft.candidate.clone().unwrap_or_else(|| configured.clone());
    let flatpak = std::env::var("FLATPAK_ID").ok().or_else(|| {
        Path::new("/.flatpak-info").exists().then(|| "com.blitzfc.qbz".into())
    });
    let snap = std::env::var("SNAP_NAME").ok();
    let (sandbox, command) = access_command(&candidate, flatpak.as_deref(), snap.as_deref());
    Snapshot {
        candidate, configured, sandbox, command, error: draft.error.clone(),
        active: crate::APP.get().and_then(|r| r.core().player().playback_storage_directory())
            .map(|path| path.to_string_lossy().into_owned()).unwrap_or_default(),
    }
}

pub(super) async fn browse() {
    if let Some(folder) = rfd::AsyncFileDialog::new().pick_folder().await {
        set(folder.path().to_string_lossy().into_owned()).await;
    }
}

fn validate_directory(parent: &str) -> Result<(), String> {
    if parent.is_empty() { return Ok(()); }
    let path = PathBuf::from(parent);
    if !path.is_absolute() || parent.contains('\0') {
        return Err(qbz_i18n::t("Choose an absolute folder path for playback storage."));
    }
    // The chosen parent must already exist: an absent external mount must not
    // cause QBZ to create a replacement tree on the system disk.
    std::fs::read_dir(&path).map_err(|e| e.to_string())?;
    let owned = path.join("qbz-playback");
    std::fs::create_dir_all(&owned).map_err(|e| e.to_string())?;
    let mut probe = tempfile::tempfile_in(&owned).map_err(|e| e.to_string())?;
    use std::io::{Read, Seek, SeekFrom, Write};
    probe.write_all(b"qbz").map_err(|e| e.to_string())?;
    probe.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut bytes = [0; 3];
    probe.read_exact(&mut bytes).map_err(|e| e.to_string())?;
    if &bytes != b"qbz" { return Err("Playback storage read-back failed".into()); }
    Ok(())
}

pub(super) async fn set(candidate: String) {
    let candidate = candidate.trim().to_string();
    let generation = {
        let mut draft = DRAFT.lock().unwrap_or_else(|e| e.into_inner());
        draft.generation += 1;
        draft.candidate = Some(candidate.clone());
        draft.error.clear();
        draft.generation
    };
    let checked = candidate.clone();
    let result = match tokio::time::timeout(std::time::Duration::from_secs(6),
        tokio::task::spawn_blocking(move || validate_directory(&checked))).await {
        Ok(result) => result.map_err(|e| e.to_string()).and_then(|r| r),
        Err(_) => Err(std::io::Error::from(std::io::ErrorKind::TimedOut).to_string()),
    };
    let mut draft = DRAFT.lock().unwrap_or_else(|e| e.into_inner());
    if draft.generation != generation { return; }
    let result = result.and_then(|_| super::with_audio(|store| {
        let mut policy = store.get_settings()?.playback_cache;
        policy.disk_directory = (!candidate.is_empty()).then(|| candidate.clone());
        store.set_playback_cache(&policy)
    }));
    draft.error = result.err().unwrap_or_default();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sandbox_commands_scope_access_and_quote_shell_metacharacters() {
        let (_, command) = access_command("/mnt/a'$(touch bad)", Some("com.blitzfc.qbz"), None);
        assert_eq!(command, "flatpak override --user --filesystem='/mnt/a'\\''$(touch bad):rw' 'com.blitzfc.qbz'");
        assert!(access_command("/mnt/music", None, Some("qbz-player")).1.contains("removable-media"));
        assert!(access_command("/srv/music", None, Some("qbz-player")).1.is_empty());
        assert!(access_command("/mntfake/music", None, Some("qbz-player")).1.is_empty());
    }
    #[test]
    fn storage_probe_requires_existing_parent_and_uses_only_owned_subdirectory() {
        let dir = tempfile::tempdir().unwrap();
        assert!(validate_directory(dir.path().join("missing").to_str().unwrap()).is_err());
        validate_directory(dir.path().to_str().unwrap()).unwrap();
        assert!(dir.path().join("qbz-playback").is_dir());
        assert_eq!(std::fs::read_dir(dir.path().join("qbz-playback")).unwrap().count(), 0);
    }
}
