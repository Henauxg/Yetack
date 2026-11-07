//! The Claude accounts a daemon runs agents with. An account is a Claude Code config folder,
//! which holds its login, its user settings and its past sessions.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use strum::VariantArray;

use crate::ids::AccountId;
use crate::stored_bool::{FromStoredBool, enum_or_stored_bool};

/// The longest account name the daemon keeps: every snapshot carries the accounts.
pub const MAX_ACCOUNT_NAME_CHARS: usize = 60;
/// A proposed folder's name: this, then the account's name made safe.
const FOLDER_PREFIX: &str = ".claude-";
/// Takes the place of each run of characters a folder name should not hold.
pub const FOLDER_SEPARATOR: char = '-';

/// The name of the folder the daemon proposes for an account, in its home folder: `.claude-`
/// and the name in lower case, each run of characters other than ASCII letters, digits, `_` and
/// `.` replaced by `-`, without a `-` or `.` at either end ("account" when nothing is left).
/// `Client Work` gives `.claude-client-work`.
pub fn account_folder_name(name: &str) -> String {
    let mut word = String::with_capacity(name.len());
    for character in name.chars() {
        if character.is_ascii_alphanumeric() || character == '_' || character == '.' {
            word.push(character.to_ascii_lowercase());
        } else if !word.ends_with(FOLDER_SEPARATOR) {
            word.push(FOLDER_SEPARATOR);
        }
    }
    let trimmed = word.trim_matches(|character| character == FOLDER_SEPARATOR || character == '.');
    let word = if trimmed.is_empty() {
        "account"
    } else {
        trimmed
    };
    format!("{FOLDER_PREFIX}{word}")
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub id: AccountId,
    pub name: String,
    /// Claude Code's config folder, absolute, on the daemon's machine. The daemon starts the
    /// account's `claude` with `CLAUDE_CONFIG_DIR` set to it, except for the default account,
    /// whose `claude` gets the daemon's own environment.
    pub folder: PathBuf,
    /// `settings.json` and the event logs of older daemons hold `use_environment_api_key`, a
    /// `bool`, in its place.
    #[serde(
        alias = "use_environment_api_key",
        deserialize_with = "enum_or_stored_bool"
    )]
    pub credentials: CredentialSource,
}

/// What the account's agents sign in to Claude with.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, VariantArray,
)]
#[serde(rename_all = "snake_case")]
pub enum CredentialSource {
    /// The folder's login. The daemon removes `ANTHROPIC_API_KEY` and `ANTHROPIC_AUTH_TOKEN`
    /// from the agents' environment.
    #[default]
    SignIn,
    /// The daemon's own `ANTHROPIC_API_KEY` and `ANTHROPIC_AUTH_TOKEN`, which Claude Code prefers
    /// to the folder's login. Older daemons and their files write `environment_api_key`.
    #[serde(alias = "environment_api_key")]
    Environment,
}

impl FromStoredBool for CredentialSource {
    fn from_stored_bool(uses_environment_api_key: bool) -> Self {
        if uses_environment_api_key {
            Self::Environment
        } else {
            Self::SignIn
        }
    }
}

/// Whether the daemon's environment has `ANTHROPIC_API_KEY` or `ANTHROPIC_AUTH_TOKEN`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, VariantArray,
)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentCredentials {
    Set,
    #[default]
    Unset,
}

/// Whether an account's folder holds a login, as far as the daemon can tell without reading it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, VariantArray)]
#[serde(rename_all = "snake_case")]
pub enum SignIn {
    /// The folder holds Claude Code's credentials file.
    SignedIn,
    /// The folder holds no credentials file.
    NotSignedIn,
    /// The folder holds no credentials file, and Claude Code may keep the login elsewhere, as in
    /// the macOS Keychain.
    Unknown,
}

/// An account as `accounts.list`, `accounts.add` and `accounts.change` answer it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountState {
    pub account: Account,
    pub sign_in: SignIn,
    /// The command that signs in to the account, typed in a terminal on the daemon's machine:
    /// for a POSIX shell, or for PowerShell on Windows.
    pub sign_in_command: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_name_keeps_only_safe_characters() {
        assert_eq!(account_folder_name("Work"), ".claude-work");
        assert_eq!(
            account_folder_name("Équipe: R&D / 2026"),
            ".claude-quipe-r-d-2026"
        );
        assert_eq!(account_folder_name("../.."), ".claude-account");
        assert_eq!(account_folder_name("my_team.v2"), ".claude-my_team.v2");
    }
}
