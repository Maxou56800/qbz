//! Release checks and signed updates, independent of the desktop and audio.
//! Package-manager installations only check: their manager owns installation.
#[cfg(target_os = "linux")]
pub mod flatpak;
pub mod install;
pub mod store;
#[cfg(windows)]
mod windows;

use chrono::{DateTime, Utc};
use semver::Version;
use serde::Deserialize;
use std::{collections::HashMap, time::Duration};

pub type Result<T> = std::result::Result<T, String>;
const RELEASES: &str = "https://api.github.com/repos/vicrodh/qbz/releases?per_page=30";
pub const RELEASE_PAGE: &str = "https://github.com/vicrodh/qbz/releases";
const MAX_METADATA: usize = 2 * 1024 * 1024;

#[derive(Clone, Debug, Deserialize)]
pub struct Release {
    pub tag_name: String,
    pub published_at: Option<DateTime<Utc>>,
    pub draft: bool,
    pub prerelease: bool,
}

impl Release {
    pub fn version(&self) -> Result<Version> {
        Version::parse(self.tag_name.strip_prefix('v').unwrap_or(&self.tag_name))
            .map_err(|e| e.to_string())
    }
    pub fn page(&self) -> String {
        format!("{RELEASE_PAGE}/tag/{}", self.tag_name)
    }

    /// Release-age policy stays in the shared updater, not in its UI adapter.
    pub fn notification_ready(&self) -> bool {
        self.published_at
            .is_some_and(|date| Utc::now().signed_duration_since(date).num_hours() >= 12)
    }
}

/// Manual checks bypass notification suppression and the automatic 12-hour delay.
pub fn select_release(
    releases: Vec<Release>,
    current: &str,
    automatic: bool,
    now: DateTime<Utc>,
) -> Result<Option<Release>> {
    let current = Version::parse(current).map_err(|e| e.to_string())?;
    Ok(releases
        .into_iter()
        .filter(|r| {
            let Ok(v) = r.version() else { return false };
            !r.draft
                && !r.prerelease
                && v.pre.is_empty()
                && v.cmp_precedence(&current).is_gt()
                && r.published_at.is_some_and(|date| {
                    let age = now.signed_duration_since(date).num_seconds();
                    age >= if automatic { 12 * 3600 } else { 0 }
                })
        })
        .max_by(|a, b| a.version().unwrap().cmp_precedence(&b.version().unwrap())))
}

pub fn client(timeout: Duration) -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .user_agent(concat!("QBZ/", env!("CARGO_PKG_VERSION")))
        .https_only(true)
        .connect_timeout(Duration::from_secs(4))
        .timeout(timeout)
        .build()
        .map_err(|e| e.to_string())
}

pub async fn metadata(url: &str) -> Result<Vec<u8>> {
    let mut response = client(Duration::from_secs(4))?
        .get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| e.to_string())?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
        if bytes.len() + chunk.len() > MAX_METADATA {
            return Err("Update metadata exceeds size limit".into());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

pub async fn check(current: &str, automatic: bool) -> Result<Option<Release>> {
    let releases = serde_json::from_slice(&metadata(RELEASES).await?).map_err(|e| e.to_string())?;
    select_release(releases, current, automatic, Utc::now())
}

#[derive(Clone, Debug, Deserialize)]
pub struct Asset {
    pub url: String,
    pub signature: String,
}
#[derive(Deserialize)]
struct Manifest {
    version: String,
    platforms: HashMap<String, Asset>,
}

pub fn select_asset(bytes: &[u8], release: &Release, platform: &str) -> Result<Asset> {
    let manifest: Manifest = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if Version::parse(&manifest.version).map_err(|e| e.to_string())? != release.version()? {
        return Err("Update manifest does not match the selected release".into());
    }
    let asset = manifest
        .platforms
        .get(platform)
        .ok_or("No signed update for this platform")?;
    let prefix = format!(
        "https://github.com/vicrodh/qbz/releases/download/{}/",
        release.tag_name
    );
    let name = asset
        .url
        .strip_prefix(&prefix)
        .ok_or("Update asset is outside the selected QBZ release")?;
    let expected = match platform {
        "linux-x86_64" => format!("QBZ_{}_amd64.AppImage", release.version()?),
        "linux-aarch64" => format!("QBZ_{}_aarch64.AppImage", release.version()?),
        "darwin-x86_64" => "QBZ_x64.app.tar.gz".into(),
        "darwin-aarch64" => "QBZ_aarch64.app.tar.gz".into(),
        "windows-x86_64" => format!("QBZ_{}_x64.msi", release.version()?),
        _ => return Err("No signed update for this platform".into()),
    };
    if name != expected || asset.signature.is_empty() {
        return Err("Invalid signed update asset".into());
    }
    Ok(asset.clone())
}

pub async fn asset(release: &Release, platform: &str) -> Result<Asset> {
    // Bind to the selected tag, never the mutable /latest alias.
    let url = format!(
        "https://github.com/vicrodh/qbz/releases/download/{}/latest.json",
        release.tag_name
    );
    select_asset(&metadata(&url).await?, release, platform)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn release(v: &str, age: i64) -> Release {
        Release {
            tag_name: format!("v{v}"),
            published_at: Some(Utc::now() - chrono::Duration::hours(age)),
            draft: false,
            prerelease: false,
        }
    }
    #[test]
    fn versions_order_semantically_and_never_downgrade() {
        let r = select_release(
            vec![release("2.9.0", 24), release("2.10.0", 24)],
            "2.1.1",
            true,
            Utc::now(),
        )
        .unwrap()
        .unwrap();
        assert_eq!(r.tag_name, "v2.10.0");
        assert!(select_release(
            vec![release("2.1.1+other", 24), release("2.0.0", 24)],
            "2.1.1",
            false,
            Utc::now()
        )
        .unwrap()
        .is_none());
    }
    #[test]
    fn automatic_waits_but_manual_finds_new_release() {
        assert!(
            select_release(vec![release("2.1.2", 1)], "2.1.1", true, Utc::now())
                .unwrap()
                .is_none()
        );
        assert!(
            select_release(vec![release("2.1.2", 1)], "2.1.1", false, Utc::now())
                .unwrap()
                .is_some()
        );
    }
    #[test]
    fn rejects_drafts_prereleases_invalid_and_future_versions() {
        let mut draft = release("3.0.0", 24);
        draft.draft = true;
        let mut pre = release("3.0.0", 24);
        pre.prerelease = true;
        assert!(select_release(
            vec![
                draft,
                pre,
                release("3.0.0-rc.1", 24),
                release("bad", 24),
                release("3.0.0", -1)
            ],
            "2.1.1",
            false,
            Utc::now()
        )
        .unwrap()
        .is_none());
    }
    #[test]
    fn manifest_cannot_switch_version_platform_or_download_origin() {
        let r = release("2.1.2", 24);
        let valid = serde_json::json!({"version":"2.1.2","platforms":{"linux-x86_64":{"signature":"sig","url":"https://github.com/vicrodh/qbz/releases/download/v2.1.2/QBZ_2.1.2_amd64.AppImage"}}});
        assert!(select_asset(&serde_json::to_vec(&valid).unwrap(), &r, "linux-x86_64").is_ok());
        assert!(select_asset(&serde_json::to_vec(&valid).unwrap(), &r, "darwin-aarch64").is_err());
        for (field, value) in [
            ("version", "2.1.3"),
            ("url", "https://evil.example/update.AppImage"),
            (
                "url",
                "https://github.com/vicrodh/qbz/releases/download/v2.1.2/../bad.AppImage",
            ),
            (
                "url",
                "https://github.com/vicrodh/qbz/releases/download/v2.1.2/QBZ_2.1.2_aarch64.AppImage",
            ),
        ] {
            let mut invalid = valid.clone();
            if field == "version" {
                invalid[field] = value.into();
            } else {
                invalid["platforms"]["linux-x86_64"][field] = value.into();
            }
            assert!(
                select_asset(&serde_json::to_vec(&invalid).unwrap(), &r, "linux-x86_64").is_err()
            );
        }
    }
    #[test]
    fn windows_msi_manifest_cannot_select_a_portable_archive() {
        let r = release("2.1.2", 24);
        let mut manifest = serde_json::json!({"version":"2.1.2","platforms":{"windows-x86_64":{"signature":"sig","url":"https://github.com/vicrodh/qbz/releases/download/v2.1.2/QBZ_2.1.2_x64.msi"}}});
        assert!(select_asset(&serde_json::to_vec(&manifest).unwrap(), &r, "windows-x86_64").is_ok());
        manifest["platforms"]["windows-x86_64"]["url"] = "https://github.com/vicrodh/qbz/releases/download/v2.1.2/qbz_2.1.2_windows_x64.zip".into();
        assert!(select_asset(&serde_json::to_vec(&manifest).unwrap(), &r, "windows-x86_64").is_err());
    }
}
