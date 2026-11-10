//! Where an account's agents send their requests: Anthropic's service, or another one that a
//! switch in the account's `settings.json` chooses, as Claude Code reads the file's `env` block.
//!
//! The same switches in the daemon's own environment would send every account's agents there, so
//! an agent's `claude` gets them only when its account takes its credentials from the
//! environment ([`ROUTE_ENV`]).
//!
//! The daemon reads only the switches: the file's other values, credentials among them, are
//! skipped as it parses, and a custom server's address is never kept.

use std::collections::BTreeSet;
use std::fmt;
use std::path::Path;

use cockpit_protocol::{Account, ClaudeRoute, CredentialSource};
use serde::de::{IgnoredAny, MapAccess, Visitor};
use serde::{Deserialize, Deserializer};
use serde_json::Value;

/// Claude Code's settings in an account's folder.
const SETTINGS_FILE: &str = "settings.json";

const BEDROCK_ENV: &str = "CLAUDE_CODE_USE_BEDROCK";
const VERTEX_ENV: &str = "CLAUDE_CODE_USE_VERTEX";
const FOUNDRY_ENV: &str = "CLAUDE_CODE_USE_FOUNDRY";
const BASE_URL_ENV: &str = "ANTHROPIC_BASE_URL";

/// The switches that choose the route, by priority.
const SWITCHES: [(&str, ClaudeRoute); 4] = [
    (BEDROCK_ENV, ClaudeRoute::Bedrock),
    (VERTEX_ENV, ClaudeRoute::Vertex),
    (FOUNDRY_ENV, ClaudeRoute::Foundry),
    (BASE_URL_ENV, ClaudeRoute::CustomServer),
];

/// The variables that send Claude Code's requests elsewhere than the account's folder login: the
/// switches, the providers' own addresses and keys. The credentials a provider takes from the
/// environment, such as `AWS_PROFILE`, stay: without a switch Claude Code ignores them, and the
/// agents' own commands may need them.
pub const ROUTE_ENV: [&str; 9] = [
    BEDROCK_ENV,
    VERTEX_ENV,
    FOUNDRY_ENV,
    BASE_URL_ENV,
    "ANTHROPIC_BEDROCK_BASE_URL",
    "ANTHROPIC_VERTEX_BASE_URL",
    "ANTHROPIC_FOUNDRY_BASE_URL",
    "AWS_BEARER_TOKEN_BEDROCK",
    "ANTHROPIC_FOUNDRY_API_KEY",
];

/// The values Claude Code reads as a switch turned on, whatever their case.
const ON_WORDS: [&str; 4] = ["1", "true", "yes", "on"];

/// The account's route: its `settings.json`'s switches, and for an account that takes its
/// credentials from the environment, the daemon's own. A missing or unreadable file sets none.
/// Reads the file: it blocks.
pub(crate) fn route(account: &Account) -> ClaudeRoute {
    let mut on = settings_switches(&account.folder.join(SETTINGS_FILE));
    if account.credentials == CredentialSource::Environment {
        on.extend(environment_switches());
    }
    route_of(&on)
}

fn route_of(on: &BTreeSet<&'static str>) -> ClaudeRoute {
    SWITCHES
        .iter()
        .find(|(name, _)| on.contains(name))
        .map_or(ClaudeRoute::Anthropic, |&(_, route)| route)
}

fn settings_switches(path: &Path) -> BTreeSet<&'static str> {
    let Ok(text) = std::fs::read(path) else {
        return BTreeSet::new();
    };
    serde_json::from_slice::<SettingsFile>(&text)
        .map(|file| file.env.0)
        .unwrap_or_default()
}

fn environment_switches() -> BTreeSet<&'static str> {
    SWITCHES
        .iter()
        .map(|&(name, _)| name)
        .filter(|name| std::env::var(name).is_ok_and(|value| is_on(name, &Value::String(value))))
        .collect()
}

/// Whether a switch's value turns it on: an address for `ANTHROPIC_BASE_URL`, one of
/// [`ON_WORDS`] for the others.
fn is_on(name: &str, value: &Value) -> bool {
    match value {
        Value::String(text) if name == BASE_URL_ENV => !text.trim().is_empty(),
        Value::String(text) => ON_WORDS
            .iter()
            .any(|word| text.trim().eq_ignore_ascii_case(word)),
        Value::Bool(on) => *on,
        Value::Number(number) => number.as_f64().is_some_and(|number| number != 0.0),
        Value::Null | Value::Array(_) | Value::Object(_) => false,
    }
}

#[derive(Default, Deserialize)]
struct SettingsFile {
    #[serde(default)]
    env: Switches,
}

/// The switches an `env` block turns on.
#[derive(Default)]
struct Switches(BTreeSet<&'static str>);

impl<'de> Deserialize<'de> for Switches {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_map(SwitchesVisitor)
    }
}

struct SwitchesVisitor;

impl<'de> Visitor<'de> for SwitchesVisitor {
    type Value = Switches;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an object of environment variables")
    }

    /// Reads the value of a switch, and skips every other value unread.
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Switches, A::Error> {
        let mut on = BTreeSet::new();
        while let Some(name) = map.next_key::<String>()? {
            match SWITCHES.iter().find(|(switch, _)| *switch == name) {
                Some(&(switch, _)) => {
                    let value: Value = map.next_value()?;
                    if is_on(switch, &value) {
                        on.insert(switch);
                    }
                }
                None => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(Switches(on))
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use cockpit_protocol::AccountId;

    use super::*;

    fn account(folder: PathBuf, credentials: CredentialSource) -> Account {
        Account {
            id: AccountId::new(1),
            name: "Work".to_owned(),
            folder,
            credentials,
        }
    }

    fn route_with(settings: &str) -> ClaudeRoute {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join(SETTINGS_FILE), settings).unwrap();
        route(&account(temp.path().to_owned(), CredentialSource::SignIn))
    }

    #[test]
    fn a_switch_in_the_settings_chooses_the_route() {
        assert_eq!(
            route_with(r#"{"env": {"CLAUDE_CODE_USE_BEDROCK": "1", "AWS_PROFILE": "work"}}"#),
            ClaudeRoute::Bedrock
        );
        assert_eq!(
            route_with(r#"{"env": {"CLAUDE_CODE_USE_VERTEX": "TRUE"}}"#),
            ClaudeRoute::Vertex
        );
        assert_eq!(
            route_with(r#"{"env": {"ANTHROPIC_BASE_URL": "https://gateway.example"}}"#),
            ClaudeRoute::CustomServer
        );
        assert_eq!(
            route_with(r#"{"env": {"CLAUDE_CODE_USE_BEDROCK": "0"}, "model": "opus"}"#),
            ClaudeRoute::Anthropic
        );
    }

    #[test]
    fn no_file_a_broken_file_or_no_switch_is_anthropic() {
        let temp = tempfile::tempdir().unwrap();
        let without = account(temp.path().join("missing"), CredentialSource::SignIn);

        assert_eq!(route(&without), ClaudeRoute::Anthropic);
        assert_eq!(route_with("{not json"), ClaudeRoute::Anthropic);
        assert_eq!(
            route_with(r#"{"env": {"AWS_PROFILE": "work"}}"#),
            ClaudeRoute::Anthropic
        );
    }

    #[test]
    fn the_settings_values_other_than_the_switches_are_not_kept() {
        let text = r#"{"env": {"AWS_SECRET_ACCESS_KEY": "made-up-secret", "CLAUDE_CODE_USE_FOUNDRY": "yes"}}"#;

        let file: SettingsFile = serde_json::from_str(text).unwrap();

        assert_eq!(file.env.0, BTreeSet::from([FOUNDRY_ENV]));
    }
}
