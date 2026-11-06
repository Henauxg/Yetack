use std::env;
use std::path::PathBuf;

use cockpit_protocol::Account;
use tokio::process::Command;

use crate::daemon_config::CLAUDE_CONFIG_DIR_ENV;

/// A login token that would take the place of any folder's login.
const OAUTH_TOKEN_ENV: &str = "CLAUDE_CODE_OAUTH_TOKEN";
/// An API key, which Claude Code prefers to the folder's login.
pub(crate) const API_KEY_ENV: &str = "ANTHROPIC_API_KEY";

/// Whether the daemon's `ANTHROPIC_API_KEY` reaches an account's `claude`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ApiKey {
    /// The variable passes as the daemon's environment has it, set or not.
    Keep,
    Remove,
}

/// What an agent's `claude` gets from its account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClaudeEnv {
    /// `CLAUDE_CONFIG_DIR`, or `None` for the default account, whose `claude` gets the daemon's
    /// own.
    pub(crate) config_dir: Option<PathBuf>,
    pub(crate) api_key: ApiKey,
}

impl ClaudeEnv {
    pub(crate) fn new(account: &Account) -> Self {
        Self {
            config_dir: (!account.id.is_default()).then(|| account.folder.clone()),
            api_key: if account.use_environment_api_key {
                ApiKey::Keep
            } else {
                ApiKey::Remove
            },
        }
    }

    /// The default account's, without the daemon's API key.
    #[cfg(test)]
    pub(crate) fn for_tests() -> Self {
        Self {
            config_dir: None,
            api_key: ApiKey::Remove,
        }
    }

    /// Sets the variables of a `claude` command. The command's other variables come from the
    /// daemon's environment.
    pub(crate) fn apply(&self, command: &mut Command) {
        command.env_remove(OAUTH_TOKEN_ENV);
        if self.api_key == ApiKey::Remove {
            command.env_remove(API_KEY_ENV);
        }
        if let Some(folder) = &self.config_dir {
            command.env(CLAUDE_CONFIG_DIR_ENV, folder);
        }
    }
}

/// Whether the daemon's environment has `ANTHROPIC_API_KEY`. Its value is never read.
pub(crate) fn environment_has_api_key() -> bool {
    env::var_os(API_KEY_ENV).is_some()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use cockpit_protocol::AccountId;

    use super::*;

    fn account(id: u32, use_environment_api_key: bool) -> Account {
        Account {
            id: AccountId::new(id),
            name: "Work".to_owned(),
            folder: PathBuf::from("/home/sam/.claude-work"),
            use_environment_api_key,
        }
    }

    /// The variables the command sets, and `None` for those it removes.
    fn changes(env: &ClaudeEnv) -> Vec<(String, Option<String>)> {
        let mut command = Command::new("claude");
        env.apply(&mut command);
        let mut changes: Vec<(String, Option<String>)> = command
            .as_std()
            .get_envs()
            .map(|(name, value)| {
                let value = value.map(|value| value.to_string_lossy().into_owned());
                (name.to_string_lossy().into_owned(), value)
            })
            .collect();
        changes.sort();
        changes
    }

    #[test]
    fn an_added_account_names_its_folder_and_leaves_out_the_daemons_logins() {
        let env = ClaudeEnv::new(&account(2, false));

        assert_eq!(
            changes(&env),
            [
                (API_KEY_ENV.to_owned(), None),
                (OAUTH_TOKEN_ENV.to_owned(), None),
                (
                    CLAUDE_CONFIG_DIR_ENV.to_owned(),
                    Some("/home/sam/.claude-work".to_owned())
                ),
            ]
        );
    }

    #[test]
    fn the_default_account_keeps_the_daemons_folder() {
        let env = ClaudeEnv::new(&account(0, false));

        assert_eq!(env.config_dir, None);
        assert_eq!(
            changes(&env),
            [
                (API_KEY_ENV.to_owned(), None),
                (OAUTH_TOKEN_ENV.to_owned(), None)
            ]
        );
    }

    #[test]
    fn an_account_that_uses_the_api_key_keeps_it_and_still_loses_the_token() {
        let env = ClaudeEnv::new(&account(2, true));

        let changes = changes(&env);

        assert!(!changes.iter().any(|(name, _)| name == API_KEY_ENV));
        assert!(changes.contains(&(OAUTH_TOKEN_ENV.to_owned(), None)));
        assert_eq!(
            env.config_dir.as_deref(),
            Some(Path::new("/home/sam/.claude-work"))
        );
    }
}
