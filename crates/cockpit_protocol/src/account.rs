//! The Claude accounts a daemon runs agents with. An account is a Claude Code config folder,
//! which holds its login, its user settings and its past sessions.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use strum::VariantArray;

use crate::ids::AccountId;

/// The longest account name the daemon keeps: every snapshot carries the accounts.
pub const MAX_ACCOUNT_NAME_CHARS: usize = 60;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub id: AccountId,
    pub name: String,
    /// Claude Code's config folder, absolute, on the daemon's machine. The daemon starts the
    /// account's `claude` with `CLAUDE_CONFIG_DIR` set to it, except for the default account,
    /// whose `claude` gets the daemon's own environment.
    pub folder: PathBuf,
    /// The account's agents get the daemon's own `ANTHROPIC_API_KEY`. Without it, the daemon
    /// removes that variable, and the agents use the folder's login.
    pub use_environment_api_key: bool,
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
