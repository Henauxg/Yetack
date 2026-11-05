use std::path::Path;
use std::time::Duration;

use cockpit_protocol::Settings;
use serde::{Deserialize, Serialize};

use crate::store::atomic_file;
use crate::store::store_error::StoreError;

/// `settings.json`: the settings clients share, and the daemon's own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub(crate) struct DaemonSettings {
    /// What `settings.set` replaces.
    #[serde(flatten)]
    pub(crate) shared: Settings,
    /// How long Claude's prompt cache lives after the last API request. A subscription gets one
    /// hour.
    pub(crate) prompt_cache_ttl_minutes: u32,
}

const DEFAULT_PROMPT_CACHE_TTL_MINUTES: u32 = 60;
const SECONDS_PER_MINUTE: u64 = 60;

impl Default for DaemonSettings {
    fn default() -> Self {
        Self {
            shared: Settings::default(),
            prompt_cache_ttl_minutes: DEFAULT_PROMPT_CACHE_TTL_MINUTES,
        }
    }
}

impl DaemonSettings {
    pub(crate) fn load(path: &Path) -> Result<Self, StoreError> {
        atomic_file::read_json(path)
    }

    pub(crate) fn save(&self, path: &Path) -> Result<(), StoreError> {
        atomic_file::write_json(path, self)
    }

    pub(crate) fn prompt_cache_ttl(&self) -> Duration {
        Duration::from_secs(u64::from(self.prompt_cache_ttl_minutes) * SECONDS_PER_MINUTE)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn round_trips_and_defaults_missing_fields() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("settings.json");
        std::fs::write(&path, br#"{"hidden_mcp_servers":["notes"]}"#).unwrap();

        let mut settings = DaemonSettings::load(&path).unwrap();
        assert_eq!(
            settings.shared.hidden_mcp_servers,
            BTreeSet::from(["notes".to_owned()])
        );
        assert_eq!(settings.prompt_cache_ttl(), Duration::from_secs(3600));

        settings.prompt_cache_ttl_minutes = 5;
        settings.save(&path).unwrap();
        assert_eq!(DaemonSettings::load(&path).unwrap(), settings);
    }
}
