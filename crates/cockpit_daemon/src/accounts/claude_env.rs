use std::env;
use std::path::PathBuf;

use cockpit_protocol::{Account, CredentialSource, EnvironmentCredentials};
use tokio::process::Command;

use crate::accounts::ROUTE_ENV;
use crate::daemon_config::CLAUDE_CONFIG_DIR_ENV;

/// A login token that would take the place of any folder's login.
const OAUTH_TOKEN_ENV: &str = "CLAUDE_CODE_OAUTH_TOKEN";
/// An API key, which Claude Code prefers to the folder's login.
pub const API_KEY_ENV: &str = "ANTHROPIC_API_KEY";
/// A token sent as a bearer token, which Claude Code also prefers to the folder's login.
pub const AUTH_TOKEN_ENV: &str = "ANTHROPIC_AUTH_TOKEN";
/// The variables an account that takes its credentials from the environment passes on.
pub const ENVIRONMENT_CREDENTIALS_ENV: [&str; 2] = [API_KEY_ENV, AUTH_TOKEN_ENV];

/// Whether the daemon's `ANTHROPIC_API_KEY` and `ANTHROPIC_AUTH_TOKEN`, and the switches of
/// [`ROUTE_ENV`], reach an account's `claude`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FromEnvironment {
    /// The variables pass as the daemon's environment has them, set or not.
    Keep,
    Remove,
}

/// What an agent's `claude` gets from its account.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClaudeEnv {
    /// `CLAUDE_CONFIG_DIR`, or `None` for the default account, whose `claude` gets the daemon's
    /// own.
    pub(crate) config_dir: Option<PathBuf>,
    pub(crate) credentials: FromEnvironment,
}

impl ClaudeEnv {
    pub(crate) fn new(account: &Account) -> Self {
        Self {
            config_dir: (!account.id.is_default()).then(|| account.folder.clone()),
            credentials: match account.credentials {
                CredentialSource::Environment => FromEnvironment::Keep,
                CredentialSource::SignIn => FromEnvironment::Remove,
            },
        }
    }

    /// The default account's, without the daemon's credentials.
    #[cfg(test)]
    pub(crate) fn for_tests() -> Self {
        Self {
            config_dir: None,
            credentials: FromEnvironment::Remove,
        }
    }

    /// Sets the variables of a `claude` command. The command's other variables come from the
    /// daemon's environment.
    pub(crate) fn apply(&self, command: &mut Command) {
        command.env_remove(OAUTH_TOKEN_ENV);
        if self.credentials == FromEnvironment::Remove {
            for name in ENVIRONMENT_CREDENTIALS_ENV.into_iter().chain(ROUTE_ENV) {
                command.env_remove(name);
            }
        }
        if let Some(folder) = &self.config_dir {
            command.env(CLAUDE_CONFIG_DIR_ENV, folder);
        }
    }
}

/// Whether the daemon's environment has `ANTHROPIC_API_KEY` or `ANTHROPIC_AUTH_TOKEN`. Their
/// values are never read.
pub(crate) fn environment_credentials() -> EnvironmentCredentials {
    if ENVIRONMENT_CREDENTIALS_ENV
        .iter()
        .any(|name| env::var_os(name).is_some())
    {
        EnvironmentCredentials::Set
    } else {
        EnvironmentCredentials::Unset
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use cockpit_protocol::AccountId;

    use super::*;

    fn account(id: u32, credentials: CredentialSource) -> Account {
        Account {
            id: AccountId::new(id),
            name: "Work".to_owned(),
            folder: PathBuf::from("/home/sam/.claude-work"),
            credentials,
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

    /// What a signed-in account's `claude` loses: the daemon's logins and route switches.
    fn removed() -> Vec<(String, Option<String>)> {
        let mut removed: Vec<(String, Option<String>)> = ENVIRONMENT_CREDENTIALS_ENV
            .into_iter()
            .chain(ROUTE_ENV)
            .chain([OAUTH_TOKEN_ENV])
            .map(|name| (name.to_owned(), None))
            .collect();
        removed.sort();
        removed
    }

    #[test]
    fn an_added_account_names_its_folder_and_leaves_out_the_daemons_logins_and_routes() {
        let env = ClaudeEnv::new(&account(2, CredentialSource::SignIn));
        let mut expected = removed();
        expected.push((
            CLAUDE_CONFIG_DIR_ENV.to_owned(),
            Some("/home/sam/.claude-work".to_owned()),
        ));
        expected.sort();

        assert_eq!(changes(&env), expected);
    }

    #[test]
    fn the_default_account_keeps_the_daemons_folder() {
        let env = ClaudeEnv::new(&account(0, CredentialSource::SignIn));

        assert_eq!(env.config_dir, None);
        assert_eq!(changes(&env), removed());
    }

    #[test]
    fn an_account_that_uses_the_environment_keeps_its_key_token_and_routes_and_loses_the_oauth_one()
    {
        let env = ClaudeEnv::new(&account(2, CredentialSource::Environment));

        let changes = changes(&env);

        for name in ENVIRONMENT_CREDENTIALS_ENV.into_iter().chain(ROUTE_ENV) {
            assert!(
                !changes.iter().any(|(changed, _)| changed == name),
                "{name}"
            );
        }
        assert!(changes.contains(&(OAUTH_TOKEN_ENV.to_owned(), None)));
        assert_eq!(
            env.config_dir.as_deref(),
            Some(Path::new("/home/sam/.claude-work"))
        );
    }
}
