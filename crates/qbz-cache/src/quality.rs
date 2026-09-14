use qbz_models::Quality;
use serde::{Deserialize, Serialize};

/// Acquisition limits, distinct from the PCM format of the available master.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheQuality {
    pub requested: Quality,
    pub resolved: Quality,
}

impl CacheQuality {
    pub fn from_resolved(requested: Quality, format_id: u32) -> Option<Self> {
        Some(Self {
            requested,
            resolved: Quality::from_id(format_id)?,
        })
    }

    pub fn satisfies(self, requested: Quality) -> bool {
        self.requested >= requested && self.resolved >= requested
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
