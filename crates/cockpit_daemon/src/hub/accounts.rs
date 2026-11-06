//! The accounts' requests, and what an agent gets from its account: its folder and its plan
//! usage.

use std::path::PathBuf;

use claude_protocol::RateLimitInfo;
use cockpit_protocol::{
    Account, AccountAddParams, AccountChangeParams, AccountId, AccountPlanUsage, AgentId,
    ErrorBody, ErrorCode, Event, ResponseBody,
};

use crate::accounts::{AccountList, SignInShell, account_state, environment_has_api_key};
use crate::daemon_config::HOME_ENV;
use crate::hub::input::Reply;
use crate::hub::state::Hub;
use crate::hub::{agents, errors};
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
        tokio::spawn(async move {
            let listed = tokio::task::spawn_blocking(move || {
                let states = accounts
                    .into_iter()
                    .map(|account| account_state(account, SignInShell::CURRENT));
                states.collect()
            })
            .await;
            let result = match listed {
                Ok(accounts) => Ok(ResponseBody::Accounts {
                    accounts,
                    environment_api_key: environment_has_api_key(),
                }),
                Err(error) => Err(errors::internal(error.to_string())),
            };
            agents::respond(reply, result);
        });
    }

    pub(super) fn accounts_add(&mut self, params: AccountAddParams, reply: Reply) {
        let mut accounts = self.settings.accounts.clone();
        let home = std::env::var_os(HOME_ENV).map(PathBuf::from);
        let added = accounts.add(params, home.as_deref(), &self.config.claude_config_dir);
        let saved = added.and_then(|account| {
            self.save_accounts(accounts)?;
            Ok(account)
        });
        answer_with_state(saved, reply);
    }

    pub(super) fn accounts_change(&mut self, params: AccountChangeParams, reply: Reply) {
        let mut accounts = self.settings.accounts.clone();
        let changed = accounts.change(params, &self.config.claude_config_dir);
        let saved = changed.and_then(|account| {
            self.save_accounts(accounts)?;
            Ok(account)
        });
        answer_with_state(saved, reply);
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
}

/// Answers with the account, whether it is signed in and how to sign in to it, from a task: the
/// sign-in check reads the folder.
fn answer_with_state(account: Result<Account, ErrorBody>, reply: Reply) {
    let account = match account {
        Ok(account) => account,
        Err(error) => return agents::respond(reply, Err(error)),
    };
    tokio::spawn(async move {
        let state =
            tokio::task::spawn_blocking(move || account_state(account, SignInShell::CURRENT)).await;
        let result = match state {
            Ok(state) => Ok(ResponseBody::Account(state)),
            Err(error) => Err(errors::internal(error.to_string())),
        };
        agents::respond(reply, result);
    });
}
