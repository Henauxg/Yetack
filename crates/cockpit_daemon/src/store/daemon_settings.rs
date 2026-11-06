use std::path::Path;
use std::time::Duration;

use cockpit_protocol::Settings;
use serde::{Deserialize, Serialize};

use crate::accounts::AccountList;
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
    /// The Claude accounts agents run with.
    pub(crate) accounts: AccountList,
}

const DEFAULT_PROMPT_CACHE_TTL_MINUTES: u32 = 60;
const SECONDS_PER_MINUTE: u64 = 60;

impl Default for DaemonSettings {
    fn default() -> Self {
        Self {
            shared: Settings::default(),
            prompt_cache_ttl_minutes: DEFAULT_PROMPT_CACHE_TTL_MINUTES,
            accounts: AccountList::default(),
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

    use serde_json::Value;

    use super::*;

    /// `settings.json` as the daemon writes it. Every value differs from its default, so a field
    /// that no longer reads back fails the test.
    const SETTINGS_JSON: &str = r#"{
  "hidden_mcp_servers": [
    "notes",
    "tracker"
  ],
  "prompt_cache_ttl_minutes": 5,
  "accounts": {
    "default_name": "Personal",
    "default_uses_environment_api_key": true,
    "added": [
      {
        "id": 2,
        "name": "Work",
        "folder": "/home/sam/.claude-work",
        "use_environment_api_key": false
      }
    ],
    "next_account": 4
  }
}
"#;

    fn sample() -> DaemonSettings {
        DaemonSettings {
            shared: Settings {
                hidden_mcp_servers: BTreeSet::from(["notes".to_owned(), "tracker".to_owned()]),
            },
            prompt_cache_ttl_minutes: 5,
            accounts: AccountList::sample(),
        }
    }

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
        assert_eq!(settings.accounts, AccountList::default());

        settings.prompt_cache_ttl_minutes = 5;
        settings.save(&path).unwrap();
        assert_eq!(DaemonSettings::load(&path).unwrap(), settings);
    }

    #[test]
    fn reads_the_saved_format() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("settings.json");
        std::fs::write(&path, SETTINGS_JSON).unwrap();

        let settings = DaemonSettings::load(&path).unwrap();

        assert_eq!(settings, sample());
    }

    #[test]
    fn writes_the_saved_format() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("settings.json");

        sample().save(&path).unwrap();

        let written: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let expected: Value = serde_json::from_str(SETTINGS_JSON).unwrap();
        assert_eq!(written, expected);
    }
}
