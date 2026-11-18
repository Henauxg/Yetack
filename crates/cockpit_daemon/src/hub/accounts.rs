//! The accounts' requests, and what an agent gets from its account: its folder, its plan usage,
//! and the notice that says how to sign in to it.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use claude_protocol::RateLimitInfo;
use cockpit_protocol::{
    Account, AccountAddParams, AccountChangeParams, AccountId, AccountPlanUsage, AccountState,
    AgentId, CredentialSource, EnvironmentCredentials, ErrorBody, ErrorCode, Event, ResponseBody,
    SignIn, SignInWait,
};

use crate::accounts::{
    API_KEY_ENV, AUTH_TOKEN_ENV, AccountList, SignInShell, account_state, environment_credentials,
    failed_sign_in, login_written, sign_in_command, sign_in_of,
};
use crate::daemon_config::HOME_ENV;
use crate::hub::input::{AccountLogin, HubInput, Reply, SignInCheck};
use crate::hub::state::Hub;
use crate::hub::{agents, errors};
use crate::process::program;
use crate::sessions::AccountFolder;
use crate::store::{DaemonSettings, LogName};
use crate::transcript::plan_usage;

impl Hub {
    pub(super) fn account(&self, id: AccountId) -> Result<Account, ErrorBody> {
        self.settings
            .accounts
            .get(id, &self.config.claude_config_dir)
            .ok_or_else(|| ErrorBody::new(ErrorCode::NotFound, format!("no account {}", id.get())))
    }

    /// Every account, the default one first.
    pub(super) fn accounts(&self) -> Vec<Account> {
        self.settings
            .accounts
            .accounts(&self.config.claude_config_dir)
    }

    /// The folder of every account, for its past sessions.
    pub(super) fn account_folders(&self) -> Vec<AccountFolder> {
        let accounts = self.accounts().into_iter();
        let folders = accounts.map(|account| AccountFolder {
            account: account.id,
            folder: account.folder,
        });
        folders.collect()
    }

    pub(super) fn accounts_list(&self, reply: Reply) {
        let accounts = self.accounts();
        let inputs = self.inputs.clone();
        let configured = self.config.claude_program.clone();
        tokio::spawn(async move {
            let checked = tokio::task::spawn_blocking(move || {
                let program = program::resolve(&configured).name;
                let mut logins = Vec::new();
                let mut states = Vec::with_capacity(accounts.len());
                for account in accounts {
                    if let Some(written) = login_written(&account) {
                        logins.push(AccountLogin {
                            account: account.id,
                            written,
                        });
                    }
                    states.push(account_state(account, SignInShell::CURRENT, &program));
                }
                (states, logins)
            })
            .await;
            let (accounts, logins) = match checked {
                Ok(checked) => checked,
                Err(error) => {
                    return agents::respond(reply, Err(errors::internal(error.to_string())));
                }
            };
            let input = HubInput::AccountsChecked {
                accounts,
                logins,
                reply,
            };
            if let Err(unsent) = inputs.send(input).await
                && let HubInput::AccountsChecked { reply, .. } = unsent.0
            {
                agents::respond(
                    reply,
                    Err(errors::internal("the daemon is stopping".to_owned())),
                );
            }
        });
    }

    /// Ends the waits the logins end, then answers `accounts.list`.
    pub(super) fn accounts_checked(
        &mut self,
        mut accounts: Vec<AccountState>,
        logins: &[AccountLogin],
        reply: Reply,
    ) {
        self.end_sign_in_waits(logins);
        let waited_on = self.accounts_waited_on();
        for state in &mut accounts {
            state.sign_in = usable(state.sign_in, waited_on.contains(&state.account.id));
        }
        agents::respond(
            reply,
            Ok(ResponseBody::Accounts {
                accounts,
                environment_credentials: environment_credentials(),
            }),
        );
    }

    /// The accounts one of whose agents waits on a sign-in: its turn found no valid login after
    /// the account's credentials file was last written, as when Claude Code could not refresh an
    /// expired OAuth session. The wait ends once the file is written again. An account whose
    /// agents get the daemon's API key or token is left out: its turns never tried the file.
    fn accounts_waited_on(&self) -> BTreeSet<AccountId> {
        let keyed = environment_credentials() == EnvironmentCredentials::Set;
        let mut waited_on = BTreeSet::new();
        for agent in self.agents.values() {
            let id = agent.record.account;
            let tries_the_file = !keyed
                || self
                    .account(id)
                    .is_ok_and(|account| account.credentials == CredentialSource::SignIn);
            if agent.view.summary().sign_in_wait == SignInWait::Waiting && tries_the_file {
                waited_on.insert(id);
            }
        }
        waited_on
    }

    /// Ends the wait of each agent whose account signed in after its turn found no login.
    fn end_sign_in_waits(&mut self, logins: &[AccountLogin]) {
        let mut signed_in: Vec<AgentId> = Vec::new();
        for agent in self.agents.values_mut() {
            let login = logins
                .iter()
                .find(|login| login.account == agent.record.account);
            if let Some(login) = login
                && agent.view.account_signed_in(login.written)
            {
                signed_in.push(agent.record.id);
            }
        }
        for agent in signed_in {
            self.publish(agent);
        }
    }

    /// Reads the accounts' folders away from the hub, unless the last check still runs.
    pub(super) fn check_sign_ins(&mut self) {
        if self.checking_sign_ins {
            return;
        }
        self.checking_sign_ins = true;
        let accounts = self.accounts();
        let inputs = self.inputs.clone();
        tokio::spawn(async move {
            let checked = tokio::task::spawn_blocking(move || {
                let mut sign_ins = BTreeMap::new();
                let mut logins = Vec::new();
                for account in &accounts {
                    sign_ins.insert(account.id, sign_in_of(account));
                    if let Some(written) = login_written(account) {
                        logins.push(AccountLogin {
                            account: account.id,
                            written,
                        });
                    }
                }
                SignInCheck { sign_ins, logins }
            })
            .await;
            if let Err(error) = &checked {
                tracing::warn!("cannot check the accounts' sign-ins: {error}");
            }
            // The hub is gone once the daemon stops.
            let _ = inputs.send(HubInput::SignInsChecked(checked.ok())).await;
        });
    }

    /// Ends the waits the check's logins end, and tells the clients when an account signed in or
    /// out since the last check, so that they ask `accounts.list` again. An account the last
    /// check did not see counts as changed: it may have signed in since its client listed it. The
    /// daemon's first check tells nothing, as a client lists the accounts when it connects.
    pub(super) fn sign_ins_checked(&mut self, check: Option<SignInCheck>) {
        self.checking_sign_ins = false;
        let Some(SignInCheck { sign_ins, logins }) = check else {
            return;
        };
        self.end_sign_in_waits(&logins);
        let waited_on = self.accounts_waited_on();
        let sign_ins: BTreeMap<AccountId, SignIn> = sign_ins
            .into_iter()
            .map(|(account, sign_in)| (account, usable(sign_in, waited_on.contains(&account))))
            .collect();
        let first = self.sign_ins.is_empty();
        let changed = !first
            && sign_ins
                .iter()
                .any(|(account, sign_in)| self.sign_ins.get(account) != Some(sign_in));
        self.sign_ins = sign_ins;
        if changed {
            let accounts = self.accounts();
            self.emit(LogName::Daemon, Event::AccountsChanged { accounts });
        }
    }

    pub(super) fn accounts_add(&mut self, params: AccountAddParams, reply: Reply) {
        let mut accounts = self.settings.accounts.clone();
        let home = std::env::var_os(HOME_ENV).map(PathBuf::from);
        let added = accounts.add(params, home.as_deref(), &self.config.claude_config_dir);
        let saved = added.and_then(|account| {
            self.save_accounts(accounts)?;
            Ok(account)
        });
        self.answer_with_state(saved, reply);
    }

    pub(super) fn accounts_change(&mut self, params: AccountChangeParams, reply: Reply) {
        let mut accounts = self.settings.accounts.clone();
        let changed = accounts.change(params, &self.config.claude_config_dir);
        let saved = changed.and_then(|account| {
            self.save_accounts(accounts)?;
            Ok(account)
        });
        self.answer_with_state(saved, reply);
    }

    /// Removes an account no agent uses, running or not: an agent keeps its account for good.
    pub(super) fn accounts_remove(&mut self, id: AccountId) -> Result<ResponseBody, ErrorBody> {
        let users: Vec<AgentId> = self
            .agents
            .values()
            .filter(|agent| agent.record.account == id)
            .map(|agent| agent.record.id)
            .collect();
        if !users.is_empty() {
            let ids: Vec<String> = users.iter().map(|agent| agent.get().to_string()).collect();
            return Err(ErrorBody::new(
                ErrorCode::AccountInUse,
                format!(
                    "agents {} use the account: close them first",
                    ids.join(", ")
                ),
            ));
        }
        let mut accounts = self.settings.accounts.clone();
        accounts.remove(id)?;
        self.save_accounts(accounts)?;
        self.plan_usage.remove(&id);
        Ok(ResponseBody::Done)
    }

    /// Saves the accounts, then applies them and tells the clients. Nothing changes when they
    /// cannot be saved.
    fn save_accounts(&mut self, accounts: AccountList) -> Result<(), ErrorBody> {
        let updated = DaemonSettings {
            accounts,
            ..self.settings.clone()
        };
        updated
            .save(&self.state.settings_file())
            .map_err(|error| errors::internal(errors::chain(&error)))?;
        self.settings = updated;
        let accounts = self.accounts();
        self.emit(LogName::Daemon, Event::AccountsChanged { accounts });
        Ok(())
    }

    /// Applies a `rate_limit_event` of one of the account's agents, and tells the clients when
    /// the usage changed: the default account's with the event an older app reads too.
    pub(super) fn plan_usage_reported(&mut self, account: AccountId, info: &RateLimitInfo) {
        let usage = self.plan_usage.entry(account).or_default();
        if !plan_usage::apply(usage, info) {
            return;
        }
        let usage = usage.clone();
        let event = if account.is_default() {
            Event::PlanUsageChanged(usage)
        } else {
            Event::AccountPlanUsageChanged(AccountPlanUsage { account, usage })
        };
        self.emit(LogName::Daemon, event);
    }

    /// What the transcript says when Claude Code finds no valid login for the account. Looks for
    /// the `claude` program on Windows, as starting an agent does.
    pub(super) fn sign_in_notice(&self, id: AccountId) -> String {
        let Ok(account) = self.account(id) else {
            return format!(
                "Claude Code could not sign in, and the agent's account {} no longer exists.",
                id.get()
            );
        };
        let program = program::resolve(&self.config.claude_program).name;
        let command = sign_in_command(&account, SignInShell::CURRENT, &program);
        let name = &account.name;
        if account.credentials == CredentialSource::Environment
            && environment_credentials() == EnvironmentCredentials::Set
        {
            return format!(
                "Claude Code refused the API key or token in the daemon's environment ({API_KEY_ENV}, {AUTH_TOKEN_ENV}), which the account {name} uses. Check them, or stop the account from using them and run this command in a terminal on the daemon's machine to sign in: {command}"
            );
        }
        format!(
            "The account {name} is not signed in, or its login expired. Run this command in a terminal on the daemon's machine to sign in, then send your message again: {command}"
        )
    }

    /// Answers with the account, whether it is signed in and how to sign in to it, from a task:
    /// the sign-in check reads the folder.
    fn answer_with_state(&self, account: Result<Account, ErrorBody>, reply: Reply) {
        let account = match account {
            Ok(account) => account,
            Err(error) => return agents::respond(reply, Err(error)),
        };
        let configured = self.config.claude_program.clone();
        let waited_on = self.accounts_waited_on().contains(&account.id);
        tokio::spawn(async move {
            let state = tokio::task::spawn_blocking(move || {
                let program = program::resolve(&configured).name;
                let mut state = account_state(account, SignInShell::CURRENT, &program);
                state.sign_in = usable(state.sign_in, waited_on);
                state
            })
            .await;
            let result = match state {
                Ok(state) => Ok(ResponseBody::Account(state)),
                Err(error) => Err(errors::internal(error.to_string())),
            };
            agents::respond(reply, result);
        });
    }
}

/// What the account's sign-in is once its agents' turns are known.
fn usable(sign_in: SignIn, waited_on: bool) -> SignIn {
    if waited_on {
        failed_sign_in(sign_in)
    } else {
        sign_in
    }
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::Frame;
    use tokio::sync::mpsc;

    use super::*;

    fn check(sign_in: SignIn) -> Option<SignInCheck> {
        Some(SignInCheck {
            sign_ins: BTreeMap::from([(AccountId::DEFAULT, sign_in)]),
            logins: Vec::new(),
        })
    }

    /// The frames the client got, as text.
    fn sent(frames: &mut mpsc::Receiver<Frame>) -> Vec<String> {
        let mut sent = Vec::new();
        while let Ok(frame) = frames.try_recv() {
            sent.push(String::from_utf8_lossy(frame.as_bytes()).into_owned());
        }
        sent
    }

    #[test]
    fn a_sign_in_or_out_tells_the_clients_once() {
        let temp = tempfile::tempdir().unwrap();
        let mut hub = Hub::for_tests(&temp.path().join("state"));
        let (client, mut frames) = mpsc::channel(16);
        hub.clients.push(client);

        hub.sign_ins_checked(check(SignIn::NotSignedIn));
        let first = sent(&mut frames);
        hub.sign_ins_checked(check(SignIn::SignedIn));
        let signed_in = sent(&mut frames);
        hub.sign_ins_checked(check(SignIn::SignedIn));
        let unchanged = sent(&mut frames);
        hub.checking_sign_ins = true;
        hub.sign_ins_checked(None);
        let failed = sent(&mut frames);

        assert!(first.is_empty(), "{first:?}");
        assert!(
            matches!(signed_in.as_slice(), [frame] if frame.contains(r#""type":"accounts_changed""#)),
            "{signed_in:?}"
        );
        assert!(unchanged.is_empty(), "{unchanged:?}");
        assert!(failed.is_empty(), "{failed:?}");
        assert!(!hub.checking_sign_ins);
        assert_eq!(
            hub.sign_ins.get(&AccountId::DEFAULT),
            Some(&SignIn::SignedIn)
        );
    }

    #[test]
    fn an_account_added_since_the_last_check_tells_the_clients_and_a_removed_one_does_not() {
        let temp = tempfile::tempdir().unwrap();
        let mut hub = Hub::for_tests(&temp.path().join("state"));
        let (client, mut frames) = mpsc::channel(16);
        hub.clients.push(client);
        let work = AccountId::new(1);
        let checked = |accounts: &[AccountId]| SignInCheck {
            sign_ins: accounts
                .iter()
                .map(|account| (*account, SignIn::NotSignedIn))
                .collect(),
            logins: Vec::new(),
        };

        hub.sign_ins_checked(Some(checked(&[AccountId::DEFAULT])));
        hub.sign_ins_checked(Some(checked(&[AccountId::DEFAULT, work])));
        let added = sent(&mut frames);
        hub.sign_ins_checked(Some(checked(&[AccountId::DEFAULT])));
        let removed = sent(&mut frames);

        assert!(
            matches!(added.as_slice(), [frame] if frame.contains(r#""type":"accounts_changed""#)),
            "{added:?}"
        );
        assert!(removed.is_empty(), "{removed:?}");
    }
}
