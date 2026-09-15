use qbz_models::Quality;
use serde::{Deserialize, Serialize};

/// Acquisition limits, distinct from the PCM format of the available master.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheQuality {
    pub requested: Quality,
    pub resolved: Quality,
    /// The request that actually succeeded, after any explicit fallback.
    /// Absent in older records: the response format alone cannot recover it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    successful_request: Option<Quality>,
}

impl CacheQuality {
    pub fn from_resolved(requested: Quality, format_id: u32) -> Option<Self> {
        Some(Self {
            requested,
            resolved: Quality::from_id(format_id)?,
            successful_request: None,
        })
    }

    pub fn from_acquisition(
        requested: Quality,
        successful_request: Quality,
        format_id: u32,
    ) -> Option<Self> {
        if successful_request > requested {
            return None;
        }
        Some(Self {
            successful_request: Some(successful_request),
            ..Self::from_resolved(requested, format_id)?
        })
    }

    pub fn satisfies(self, requested: Quality) -> bool {
        if self.requested < requested || (requested != Quality::Mp3 && self.resolved == Quality::Mp3) {
            return false;
        }
        match self.successful_request {
            // A successful highest-tier request can return a CD or 96 kHz
            // master. Only an explicit lower-tier request limits reuse.
            Some(actual) => actual <= self.requested && actual >= requested,
            None => self.resolved >= requested,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn successful_request_reuses_available_cd_and_96khz_masters() {
        for response_format in [6, 7, 27] {
            let acquired = CacheQuality::from_acquisition(
                Quality::UltraHiRes, Quality::UltraHiRes, response_format,
            ).unwrap();
            for target in Quality::fallback_order() {
                assert!(acquired.satisfies(*target));
            }
        }
    }

    #[test]
    fn successful_request_preserves_explicit_fallback_and_lossy_limits() {
        for actual in Quality::fallback_order() {
            let acquired = CacheQuality::from_acquisition(
                Quality::UltraHiRes, *actual, actual.id(),
            ).unwrap();
            for target in Quality::fallback_order() {
                assert_eq!(acquired.satisfies(*target), actual >= target);
            }
        }
        // Even an unexpected lossy response to a lossless request must not
        // qualify as the best lossless master.
        let lossy = CacheQuality::from_acquisition(Quality::UltraHiRes, Quality::UltraHiRes, 5).unwrap();
        assert!(!lossy.satisfies(Quality::Lossless));
        assert!(lossy.satisfies(Quality::Mp3));
        assert!(CacheQuality::from_acquisition(Quality::HiRes, Quality::UltraHiRes, 7).is_none());
        assert!(CacheQuality::from_acquisition(Quality::UltraHiRes, Quality::UltraHiRes, 999).is_none());
    }

    #[test]
    fn successful_request_missing_or_inconsistent_remains_conservative() {
        let old: CacheQuality = serde_json::from_str(
            r#"{"requested":"UltraHiRes","resolved":"HiRes"}"#,
        ).unwrap();
        assert!(!old.satisfies(Quality::UltraHiRes));
        assert!(old.satisfies(Quality::HiRes));
        let inconsistent: CacheQuality = serde_json::from_str(
            r#"{"requested":"HiRes","resolved":"HiRes","successful_request":"UltraHiRes"}"#,
        ).unwrap();
        assert!(!inconsistent.satisfies(Quality::HiRes));
    }

    #[test]
    fn acquisition_ceiling_distinguishes_best_master_from_lower_tier_copy() {
        let best = CacheQuality::from_resolved(Quality::UltraHiRes, 27).unwrap();
        assert!(best.satisfies(Quality::UltraHiRes));
        assert!(best.satisfies(Quality::HiRes));
        let limited = CacheQuality::from_resolved(Quality::HiRes, 7).unwrap();
        assert!(!limited.satisfies(Quality::UltraHiRes));
        let fallback = CacheQuality::from_resolved(Quality::UltraHiRes, 7).unwrap();
        assert!(!fallback.satisfies(Quality::UltraHiRes));
        assert!(CacheQuality::from_resolved(Quality::UltraHiRes, 999).is_none());
    }
}
