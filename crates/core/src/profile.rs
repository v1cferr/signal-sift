//! Execution profiles: how much of the machine SignalSift may use right now.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Execution profile, switched manually for now (no game detection yet).
///
/// The profile gates *starting* work. A heavy job that is already running when
/// the profile changes is cancelled by the worker and resumes from its last
/// checkpoint when the profile allows heavy work again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Profile {
    /// Normal collection; heavy processing allowed within the concurrency limit.
    #[default]
    Balanced,
    /// API and light collection keep running; no new transcription or inference.
    Gaming,
    /// API and minimal state only; collection and processing are deferred.
    Eco,
}

/// Classes of work the scheduler distinguishes when applying a profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkClass {
    /// Feed polling, metadata and subtitle downloads: network-bound, cheap.
    LightCollection,
    /// Speech recognition and LLM inference: CPU/GPU-bound, expensive.
    Heavy,
}

impl Profile {
    pub fn allows(self, work: WorkClass) -> bool {
        match (self, work) {
            (Profile::Balanced, _) => true,
            (Profile::Gaming, WorkClass::LightCollection) => true,
            (Profile::Gaming, WorkClass::Heavy) => false,
            (Profile::Eco, _) => false,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Profile::Balanced => "balanced",
            Profile::Gaming => "gaming",
            Profile::Eco => "eco",
        }
    }
}

impl fmt::Display for Profile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, thiserror::Error)]
#[error("unknown profile {0:?}; expected balanced, gaming or eco")]
pub struct UnknownProfile(String);

impl FromStr for Profile {
    type Err = UnknownProfile;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "balanced" => Ok(Profile::Balanced),
            "gaming" => Ok(Profile::Gaming),
            "eco" => Ok(Profile::Eco),
            other => Err(UnknownProfile(other.to_owned())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gaming_keeps_light_collection_but_blocks_heavy_work() {
        assert!(Profile::Gaming.allows(WorkClass::LightCollection));
        assert!(!Profile::Gaming.allows(WorkClass::Heavy));
    }

    #[test]
    fn eco_defers_everything() {
        assert!(!Profile::Eco.allows(WorkClass::LightCollection));
        assert!(!Profile::Eco.allows(WorkClass::Heavy));
    }

    #[test]
    fn round_trips_through_strings() {
        for p in [Profile::Balanced, Profile::Gaming, Profile::Eco] {
            assert_eq!(p.as_str().parse::<Profile>().unwrap(), p);
        }
        assert!("turbo".parse::<Profile>().is_err());
    }
}
