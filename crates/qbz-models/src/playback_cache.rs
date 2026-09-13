//! Persisted playback-memory policy shared by GUI and headless hosts.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaybackMemoryProfile {
    Auto,
    High,
    Desktop,
    Low,
    Custom,
}

impl PlaybackMemoryProfile {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::High => "high",
            Self::Desktop => "desktop",
            Self::Low => "low",
            Self::Custom => "custom",
        }
    }
    pub fn parse(value: &str) -> Result<Self, String> {
        match value {
            "auto" => Ok(Self::Auto),
            "high" => Ok(Self::High),
            "desktop" => Ok(Self::Desktop),
            "low" => Ok(Self::Low),
            "custom" => Ok(Self::Custom),
            _ => Err("Unknown playback memory profile".into()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlaybackPrefetchPolicy {
    pub lookahead: usize,
    pub concurrency: usize,
    pub allow_hires: bool,
    pub initial_buffer_cap: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PlaybackCacheSettings {
    /// Missing in older settings: default limits map to Auto, edited limits to Custom.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<PlaybackMemoryProfile>,
    pub dynamic: bool,
    /// None keeps the host's recommended base (400 MiB, or 50 MiB low-memory).
    pub min_mib: Option<u32>,
    /// None uses the base in fixed mode, or a bounded automatic ceiling.
    pub max_mib: Option<u32>,
    /// Optional parent directory; QBZ owns only its qbz-playback child.
    /// Separate from the memory preset and applied when Player is constructed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk_directory: Option<String>,
}

impl PlaybackCacheSettings {
    pub fn selected_profile(&self) -> PlaybackMemoryProfile {
        self.profile.unwrap_or_else(|| {
            if self.dynamic || self.min_mib.is_some() || self.max_mib.is_some() {
                PlaybackMemoryProfile::Custom
            } else {
                PlaybackMemoryProfile::Auto
            }
        })
    }

    pub fn select_profile(&mut self, profile: PlaybackMemoryProfile) {
        if profile == PlaybackMemoryProfile::Custom {
            self.profile = Some(profile);
            return;
        }
        let directory = self.disk_directory.take();
        *self = Self { disk_directory: directory, ..Self::default() };
        match profile {
            PlaybackMemoryProfile::High => {
                self.dynamic = true;
                self.min_mib = Some(400);
                self.max_mib = Some(1600);
            }
            PlaybackMemoryProfile::Desktop => {
                self.min_mib = Some(400);
                self.max_mib = None;
            }
            PlaybackMemoryProfile::Low => {
                self.min_mib = Some(50);
                self.max_mib = None;
            }
            _ => {}
        }
        if profile != PlaybackMemoryProfile::Auto {
            self.profile = Some(profile);
        }
    }

    pub fn make_custom(&mut self) {
        self.profile = Some(PlaybackMemoryProfile::Custom);
    }

    pub fn prefetch_policy(&self, host_recommended_bytes: usize) -> PlaybackPrefetchPolicy {
        let low = match self.selected_profile() {
            PlaybackMemoryProfile::Low => true,
            PlaybackMemoryProfile::High | PlaybackMemoryProfile::Desktop => false,
            _ => host_recommended_bytes <= 50 * 1024 * 1024,
        };
        PlaybackPrefetchPolicy {
            lookahead: if low { 1 } else { 5 },
            concurrency: if low { 1 } else { 2 },
            allow_hires: !low,
            initial_buffer_cap: if low { 256 * 1024 } else { 2 * 1024 * 1024 },
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        if let Some(path) = &self.disk_directory {
            if !std::path::Path::new(path).is_absolute() || path.contains('\0') {
                return Err("Choose an absolute folder path for playback storage.".into());
            }
        }
        if let Some(profile) = self.profile.filter(|p| *p != PlaybackMemoryProfile::Custom) {
            let mut canonical = Self::default();
            canonical.select_profile(profile);
            if (self.dynamic, self.min_mib, self.max_mib)
                != (canonical.dynamic, canonical.min_mib, canonical.max_mib)
            {
                return Err("Playback memory preset conflicts with custom cache limits".into());
            }
        }
        for value in [self.min_mib, self.max_mib].into_iter().flatten() {
            if !(16..=16384).contains(&value) {
                return Err("Playback cache limits must be between 16 and 16384 MiB".into());
            }
        }
        if matches!((self.min_mib, self.max_mib), (Some(min), Some(max)) if min > max) {
            return Err("Minimum playback cache budget must not exceed its maximum".into());
        }
        Ok(())
    }

    pub fn budgets(&self, recommended_bytes: usize) -> (usize, usize) {
        const MIB: usize = 1024 * 1024;
        let base = self
            .min_mib
            .map(|m| (m as u64 * MIB as u64).min(usize::MAX as u64) as usize)
            .unwrap_or(recommended_bytes);
        let ceiling = self
            .max_mib
            .map(|m| (m as u64 * MIB as u64).min(usize::MAX as u64) as usize)
            .unwrap_or_else(|| {
                if self.dynamic {
                    base.saturating_mul(4)
                        .min((16384u64 * MIB as u64).min(usize::MAX as u64) as usize)
                } else {
                    base
                }
            });
        (base.min(ceiling), ceiling)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switching_memory_profiles_preserves_playback_storage() {
        let mut policy = PlaybackCacheSettings::default();
        let path = std::env::temp_dir().join("playback-drive").to_string_lossy().into_owned();
        policy.disk_directory = Some(path.clone());
        for profile in [PlaybackMemoryProfile::High, PlaybackMemoryProfile::Low,
            PlaybackMemoryProfile::Desktop, PlaybackMemoryProfile::Custom, PlaybackMemoryProfile::Auto] {
            policy.select_profile(profile);
            assert_eq!(policy.disk_directory.as_deref(), Some(path.as_str()));
            policy.validate().unwrap();
            let restored: PlaybackCacheSettings = serde_json::from_str(&serde_json::to_string(&policy).unwrap()).unwrap();
            assert_eq!(restored.disk_directory, policy.disk_directory);
        }
    }
    #[test]
    fn legacy_settings_keep_recommended_fixed_budget() {
        let policy: PlaybackCacheSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(policy, PlaybackCacheSettings::default());
        assert_eq!(policy.budgets(400 << 20), (400 << 20, 400 << 20));
        assert_eq!(policy.budgets(50 << 20), (50 << 20, 50 << 20));
    }
    #[test]
    fn invalid_bounds_are_rejected_and_dynamic_ceiling_is_bounded() {
        let mut p = PlaybackCacheSettings {
            dynamic: true,
            min_mib: Some(400),
            max_mib: None,
            ..Default::default()
        };
        assert_eq!(p.budgets(50 << 20), (400 << 20, 1600 << 20));
        for max in [0, 15, 399, 16385, u32::MAX] {
            p.max_mib = Some(max);
            assert!(p.validate().is_err());
        }
        p.max_mib = Some(1600);
        assert!(p.validate().is_ok());
    }
}

#[cfg(test)]
mod profile_tests {
    use super::*;
    #[test]
    fn memory_profiles_roundtrip_and_apply_on_both_host_classes() {
        for (profile, base, max, dynamic) in [
            (PlaybackMemoryProfile::High, 400, 1600, true),
            (PlaybackMemoryProfile::Desktop, 400, 400, false),
            (PlaybackMemoryProfile::Low, 50, 50, false),
        ] {
            let mut p = PlaybackCacheSettings::default();
            p.select_profile(profile);
            p.validate().unwrap();
            for host in [50, 400] {
                assert_eq!(p.budgets(host << 20), (base << 20, max << 20));
            }
            assert_eq!(p.dynamic, dynamic);
            assert_eq!(
                serde_json::from_str::<PlaybackCacheSettings>(&serde_json::to_string(&p).unwrap())
                    .unwrap(),
                p
            );
            assert_eq!(
                p.prefetch_policy(400 << 20).allow_hires,
                profile != PlaybackMemoryProfile::Low
            );
        }
    }
    #[test]
    fn memory_profiles_preserve_legacy_custom_and_reset_to_host_auto() {
        let mut p: PlaybackCacheSettings =
            serde_json::from_str(r#"{"dynamic":true,"min_mib":512,"max_mib":2048}"#).unwrap();
        assert_eq!(p.selected_profile(), PlaybackMemoryProfile::Custom);
        assert_eq!(p.budgets(50 << 20), (512 << 20, 2048 << 20));
        p.select_profile(PlaybackMemoryProfile::Low);
        p.make_custom();
        p.min_mib = Some(32);
        p.validate().unwrap();
        assert_eq!(p.selected_profile(), PlaybackMemoryProfile::Custom);
        p.select_profile(PlaybackMemoryProfile::Auto);
        assert_eq!(p, PlaybackCacheSettings::default());
        assert_eq!(p.budgets(50 << 20), (50 << 20, 50 << 20));
        assert_eq!(p.prefetch_policy(50 << 20).concurrency, 1);
        assert_eq!(p.prefetch_policy(400 << 20).concurrency, 2);
    }
    #[test]
    fn memory_profiles_reject_conflicting_presets_and_unknown_names() {
        let mut p = PlaybackCacheSettings::default();
        p.select_profile(PlaybackMemoryProfile::High);
        p.dynamic = false;
        assert!(p.validate().is_err());
        assert!(PlaybackMemoryProfile::parse("unlimited").is_err());
        assert!(
            serde_json::from_str::<PlaybackCacheSettings>(r#"{"profile":"unlimited"}"#).is_err()
        );
    }
}
