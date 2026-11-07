//! The Claude accounts in the window: their colors and labels, the new agent dialog's account,
//! the plan chip and card, the resume list's filter, an agent that waits for a sign-in, and the
//! "Add an account" and "Accounts" dialogs.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::SystemTime;

use cockpit_protocol::{
    Account, AccountAddParams, AccountChangeParams, AccountId, AccountParams, AccountState,
    AgentId, AgentStatus, AgentSummary, FOLDER_SEPARATOR, ItemBody, ItemId, PlanUsage, Prompt,
    PromptParams, Request, ResponseBody, SignIn, UsageWindow, UserMessage, account_folder_name,
};

use super::app_state::{AgentEntry, AppState};
use super::drafts::Drafts;
use super::requests::{Outgoing, Purpose};
use super::ui_state::{Dialog, NewAgentForm};
use crate::format;
use crate::path_style::PathStyle;
use crate::prefs::AccountColor;

/// The name of Claude Code's own config folder, in the home folder.
const CLAUDE_FOLDER: &str = ".claude";

/// What `accounts.list` last answered.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SignIns {
    pub states: BTreeMap<AccountId, AccountState>,
    /// Whether the daemon's environment has `ANTHROPIC_API_KEY`.
    pub environment_api_key: bool,
    /// When the daemon last answered. `None` asks it again.
    pub checked_at: Option<SystemTime>,
    /// An `accounts.list` is on its way.
    pub checking: bool,
    /// Something changed while the daemon was asked: its answer may predate it.
    pub ask_again: bool,
}

impl SignIns {
    pub fn sign_in(&self, account: AccountId) -> Option<SignIn> {
        self.states.get(&account).map(|state| state.sign_in)
    }

    pub fn command(&self, account: AccountId) -> Option<&str> {
        self.states
            .get(&account)
            .map(|state| state.sign_in_command.as_str())
    }

    /// Asks the daemon again, at once or once its answer on the way arrives.
    pub fn refresh(&mut self) {
        if self.checking {
            self.ask_again = true;
        } else {
            self.checked_at = None;
        }
    }
}

/// An account's name in its color, as the sidebar, the header and the lists show it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountLabel {
    pub id: AccountId,
    pub name: String,
    pub color: AccountColor,
}

/// Labels show once there is a choice: two accounts or more.
pub fn shows_labels(state: &AppState) -> bool {
    state.accounts.len() >= 2
}

pub fn account(state: &AppState, id: AccountId) -> Option<&Account> {
    state.accounts.iter().find(|account| account.id == id)
}

pub fn color(state: &AppState, id: AccountId) -> AccountColor {
    state.prefs.accounts.color(&state.daemon.machine, id)
}

/// The account's label, whatever the number of accounts.
pub fn label_of(state: &AppState, id: AccountId) -> Option<AccountLabel> {
    let account = account(state, id)?;
    Some(AccountLabel {
        id,
        name: account.name.clone(),
        color: color(state, id),
    })
}

/// The label an agent, a folder or a session shows: `None` with fewer than two accounts.
pub fn label(state: &AppState, id: AccountId) -> Option<AccountLabel> {
    shows_labels(state).then(|| label_of(state, id)).flatten()
}

/// Every account's label, the default one first.
pub fn labels(state: &AppState) -> Vec<AccountLabel> {
    state
        .accounts
        .iter()
        .filter_map(|account| label_of(state, account.id))
        .collect()
}

/// How an account's agents pay for Claude.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Billing {
    /// A Claude subscription's plan, with its windows. Also an account whose sign-in the daemon
    /// cannot tell, or has not told yet.
    Plan,
    /// The daemon's `ANTHROPIC_API_KEY`: no plan limits.
    ApiKey,
    NotSignedIn,
}

pub fn billing(state: &AppState, id: AccountId) -> Billing {
    if account(state, id).is_some_and(|account| account.use_environment_api_key) {
        return Billing::ApiKey;
    }
    match state.sign_ins.sign_in(id) {
        Some(SignIn::NotSignedIn) => Billing::NotSignedIn,
        Some(SignIn::SignedIn | SignIn::Unknown) | None => Billing::Plan,
    }
}

/// The account's plan usage: the default account's comes apart from the others'.
pub fn plan_usage(state: &AppState, id: AccountId) -> Option<&PlanUsage> {
    if id.is_default() {
        return Some(&state.plan_usage);
    }
    state.other_plan_usage.get(&id)
}

pub fn window_percent(usage: Option<&PlanUsage>, window: UsageWindow) -> Option<u8> {
    let utilization = usage?.windows.get(&window)?.utilization?;
    Some(format::percent(utilization))
}

/// The five-hour window, which the chip shows; another window when Claude Code reported only
/// that one.
pub fn plan_percent(usage: &PlanUsage) -> Option<u8> {
    let any = || usage.windows.values().find_map(|window| window.utilization);
    window_percent(Some(usage), UsageWindow::FiveHour).or_else(|| any().map(format::percent))
}

/// What the header's Plan chip shows for the selected agent's account.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanChip {
    /// The plan's percent, `None` while Claude Code has reported none.
    Percent(Option<u8>),
    ApiKey,
    NotSignedIn,
}

pub fn plan_chip(state: &AppState, id: AccountId) -> PlanChip {
    match billing(state, id) {
        Billing::ApiKey => PlanChip::ApiKey,
        Billing::NotSignedIn => PlanChip::NotSignedIn,
        Billing::Plan => PlanChip::Percent(plan_usage(state, id).and_then(plan_percent)),
    }
}

/// The plan card's accounts: the agent's first, then the others in the daemon's order.
pub fn plan_card_accounts(state: &AppState, agent_account: AccountId) -> Vec<AccountId> {
    let mut ids: Vec<AccountId> = Vec::with_capacity(state.accounts.len());
    if account(state, agent_account).is_some() {
        ids.push(agent_account);
    }
    let others = state
        .accounts
        .iter()
        .map(|account| account.id)
        .filter(|&id| id != agent_account);
    ids.extend(others);
    ids
}

/// The account of the last agent or past session in `folder`, by the daemon machine's path
/// rules.
pub fn folder_account(state: &AppState, folder: &str) -> Option<AccountId> {
    let folder = folder.trim();
    if folder.is_empty() {
        return None;
    }
    let style = PathStyle::of_text(folder);
    state
        .recent_folders()
        .into_iter()
        .find(|recent| style.same_path(&recent.folder.to_string_lossy(), folder))
        .map(|recent| recent.account)
}

/// The account a new agent in `folder` runs with: the one picked, else the folder's last one,
/// else the default one. An account the daemon no longer has counts as none.
pub fn new_agent_account(state: &AppState, form: &NewAgentForm, folder: &str) -> AccountId {
    let known = |id: &AccountId| account(state, *id).is_some();
    form.account
        .filter(known)
        .or_else(|| folder_account(state, folder).filter(known))
        .unwrap_or(AccountId::DEFAULT)
}

/// The account menu's title: why the button shows the account it shows.
pub fn picker_title(state: &AppState, folder: &str) -> String {
    let last = folder_account(state, folder).and_then(|id| account(state, id));
    match last {
        Some(last) => format!("Account · {} ran the last agent in this folder", last.name),
        None => "Account · the default account for a new folder".to_owned(),
    }
}

/// What the account menu says under an account's name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChoiceDetail {
    Plan {
        five_hour: Option<u8>,
        seven_day: Option<u8>,
    },
    ApiKey,
    NotSignedIn,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountChoice {
    pub label: AccountLabel,
    pub detail: ChoiceDetail,
}

pub fn choices(state: &AppState) -> Vec<AccountChoice> {
    let all = labels(state).into_iter();
    let choices = all.map(|label| {
        let detail = match billing(state, label.id) {
            Billing::ApiKey => ChoiceDetail::ApiKey,
            Billing::NotSignedIn => ChoiceDetail::NotSignedIn,
            Billing::Plan => {
                let usage = plan_usage(state, label.id);
                ChoiceDetail::Plan {
                    five_hour: window_percent(usage, UsageWindow::FiveHour),
                    seven_day: window_percent(usage, UsageWindow::SevenDay),
                }
            }
        };
        AccountChoice { label, detail }
    });
    choices.collect()
}

/// The new agent dialog's account menu picks an account for good, until a recent folder is
/// picked.
pub fn pick_new_agent_account(state: &mut AppState, id: AccountId) {
    if let Some(Dialog::NewAgent(form)) = &mut state.ui.dialog {
        form.account = Some(id);
        form.menu = None;
    }
}

/// A recent folder picked: the account follows the folder again.
pub fn follow_folder_account(state: &mut AppState) {
    if let Some(Dialog::NewAgent(form)) = &mut state.ui.dialog {
        form.account = None;
    }
}

/// The resume list's filter: every account's sessions, or one account's.
pub fn set_session_account(drafts: &mut Drafts, account: Option<AccountId>) {
    drafts.new_agent.session_account = account;
    drafts.new_agent.session_highlighted = 0;
}

/// Shows one account's sessions; a click on the chosen account shows them all again.
pub fn toggle_session_account(drafts: &mut Drafts, id: AccountId) {
    let shown = (drafts.new_agent.session_account != Some(id)).then_some(id);
    set_session_account(drafts, shown);
}

/// The turn of an agent that found no login, while no message went after it: the chat shows it
/// as a card, in place of its notice and its turn end.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignInTurn {
    pub turn_end: ItemId,
    /// The daemon's notice, which says the same in text.
    pub notice: Option<ItemId>,
    /// The message Claude Code did not get.
    pub message: Option<UserMessage>,
}

pub fn sign_in_turn(entry: &AgentEntry) -> Option<SignInTurn> {
    let items: Vec<_> = entry
        .transcript
        .items()
        .filter(|item| item.parent.is_none())
        .collect();
    let end = items.iter().rposition(|item| match &item.body {
        ItemBody::TurnEnd(_) => true,
        ItemBody::UserMessage(message) => !message.queued,
        _ => false,
    })?;
    let turn_end = items.get(end)?;
    let ItemBody::TurnEnd(ended) = &turn_end.body else {
        return None;
    };
    if !ended.not_signed_in {
        return None;
    }
    let before = items.get(..end).unwrap_or_default();
    let notice = before
        .last()
        .filter(|item| matches!(item.body, ItemBody::Notice { .. }))
        .map(|item| item.id);
    let message = before.iter().rev().find_map(|item| match &item.body {
        ItemBody::UserMessage(message) if !message.queued => Some(message.clone()),
        _ => None,
    });
    Some(SignInTurn {
        turn_end: turn_end.id,
        notice,
        message,
    })
}

/// Whether "Send the message again" sends: the account signed in, the app holds the message,
/// and the card has not sent it yet.
pub fn can_send_again(entry: &AgentEntry, turn: &SignInTurn) -> bool {
    !entry.summary.waits_for_sign_in
        && turn.message.is_some()
        && entry.sent_again != Some(turn.turn_end)
}

/// Sends the message of the turn that found no login again, once.
pub fn send_again(state: &mut AppState, agent: AgentId) -> Vec<Outgoing> {
    let Some(entry) = state.agents.get_mut(&agent) else {
        return Vec::new();
    };
    let Some(turn) = sign_in_turn(entry).filter(|turn| can_send_again(entry, turn)) else {
        return Vec::new();
    };
    let Some(message) = turn.message else {
        return Vec::new();
    };
    entry.sent_again = Some(turn.turn_end);
    state.ui.chat_scrolled_up.remove(&agent);
    state.ui.chat_jumps = state.ui.chat_jumps.wrapping_add(1);
    let prompt = Prompt {
        text: message.text,
        images: message.images,
    };
    vec![Outgoing {
        request: Request::AgentPrompt(PromptParams { agent, prompt }),
        purpose: Purpose::SendAgain {
            agent,
            turn_end: turn.turn_end,
        },
    }]
}

/// The message did not go: the card offers to send it again.
pub fn send_again_failed(state: &mut AppState, agent: AgentId, turn_end: ItemId, error: &str) {
    if let Some(entry) = state.agents.get_mut(&agent) {
        entry.sent_again.take_if(|sent| *sent == turn_end);
    }
    state
        .ui
        .report(format!("The message was not sent: {error}"));
}

/// The agent's status as its turn sees it: an agent that waits only for its account's sign-in
/// has no turn running, so the composer sends, and nothing stops or is lost on close.
pub fn turn_status(summary: &AgentSummary) -> AgentStatus {
    let only_sign_in = summary.waits_for_sign_in && summary.pending_count == 0;
    if only_sign_in && summary.status == AgentStatus::NeedsYou {
        return AgentStatus::Idle;
    }
    summary.status
}

/// The sidebar's line for an agent that waits for its account's sign-in.
pub fn waiting_line(state: &AppState, entry: &AgentEntry) -> Option<String> {
    if !entry.summary.waits_for_sign_in {
        return None;
    }
    let line = match account(state, entry.summary.account) {
        Some(account) => format!("{} is not signed in", account.name),
        None => "Not signed in".to_owned(),
    };
    Some(line)
}

/// How many agents besides `agent` wait for the same account's sign-in.
pub fn others_waiting(state: &AppState, agent: AgentId, id: AccountId) -> usize {
    let others = state.agents.values().filter(|entry| {
        entry.id() != agent && entry.summary.account == id && entry.summary.waits_for_sign_in
    });
    others.count()
}

/// The agents of an account, running or not.
pub fn agent_count(state: &AppState, id: AccountId) -> usize {
    let agents = state.agents.values();
    agents.filter(|entry| entry.summary.account == id).count()
}

/// `accounts.list`, when the window needs the sign-in states and they are not on their way.
pub fn list_request(state: &mut AppState) -> Option<Outgoing> {
    let sign_ins = &mut state.sign_ins;
    if state.accounts.is_empty() || sign_ins.checked_at.is_some() || sign_ins.checking {
        return None;
    }
    sign_ins.checking = true;
    Some(Outgoing {
        request: Request::AccountsList,
        purpose: Purpose::Accounts,
    })
}

/// "Check again": the daemon looks at the accounts' folders, and ends the waits of the agents
/// whose account signed in since.
pub fn check_again(state: &mut AppState) {
    state.sign_ins.refresh();
}

/// The "Accounts" dialog: which card is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AccountsView {
    pub expanded: Option<AccountId>,
}

pub fn open_accounts(state: &mut AppState, expanded: Option<AccountId>) {
    state.ui.popover = None;
    state.ui.dialog = Some(Dialog::Accounts(AccountsView { expanded }));
    state.sign_ins.refresh();
}

pub fn toggle_account(state: &mut AppState, id: AccountId) {
    if let Some(Dialog::Accounts(view)) = &mut state.ui.dialog {
        view.expanded = (view.expanded != Some(id)).then_some(id);
    }
}

pub fn set_color(state: &mut AppState, id: AccountId, color: AccountColor) {
    let machine = state.daemon.machine.clone();
    state.prefs.accounts.set_color(&machine, id, color);
}

/// Renames the account, when the name changed and is not empty.
pub fn rename_account(state: &AppState, id: AccountId, name: &str) -> Vec<Outgoing> {
    let name = name.trim();
    let unchanged = account(state, id).is_none_or(|account| account.name == name);
    if name.is_empty() || unchanged {
        return Vec::new();
    }
    change_request(AccountChangeParams {
        account: id,
        name: Some(name.to_owned()),
        use_environment_api_key: None,
    })
}

pub fn set_api_key(id: AccountId, uses: bool) -> Vec<Outgoing> {
    change_request(AccountChangeParams {
        account: id,
        name: None,
        use_environment_api_key: Some(uses),
    })
}

fn change_request(params: AccountChangeParams) -> Vec<Outgoing> {
    vec![Outgoing {
        request: Request::AccountsChange(params),
        purpose: Purpose::AccountChanged,
    }]
}

/// Why an account cannot be removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoveBlock {
    /// The default account is the login `claude` has without the cockpit.
    Default,
    /// Agents use it, running or not: an agent keeps its account.
    InUse(usize),
}

pub fn remove_block(state: &AppState, id: AccountId) -> Option<RemoveBlock> {
    if id.is_default() {
        return Some(RemoveBlock::Default);
    }
    let count = agent_count(state, id);
    (count > 0).then_some(RemoveBlock::InUse(count))
}

pub fn remove_account(state: &mut AppState, id: AccountId) -> Vec<Outgoing> {
    if remove_block(state, id).is_some() {
        return Vec::new();
    }
    if let Some(Dialog::Accounts(view)) = &mut state.ui.dialog {
        view.expanded.take_if(|expanded| *expanded == id);
    }
    vec![Outgoing::action(Request::AccountsRemove(AccountParams {
        account: id,
    }))]
}

/// The "Add an account" dialog. Its text fields are in the drafts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddAccountForm {
    pub color: AccountColor,
    pub use_api_key: bool,
    /// The `accounts.add` request is on its way.
    pub adding: bool,
    /// The account, once the daemon added it: the dialog then shows how to sign in to it.
    pub added: Option<AccountId>,
    /// Why the daemon refused the account.
    pub error: Option<String>,
}

pub fn open_add_account(state: &mut AppState, drafts: &mut Drafts) {
    state.ui.popover = None;
    state.ui.dialog = Some(Dialog::AddAccount(AddAccountForm {
        color: free_color(state),
        use_api_key: false,
        adding: false,
        added: None,
        error: None,
    }));
    drafts.add_account = Default::default();
}

/// The first color no account has, else the one the next id would take.
pub fn free_color(state: &AppState) -> AccountColor {
    use strum::VariantArray;

    let taken: Vec<AccountColor> = state
        .accounts
        .iter()
        .map(|account| color(state, account.id))
        .collect();
    let free = AccountColor::VARIANTS
        .iter()
        .copied()
        .find(|color| !taken.contains(color));
    let next = state.accounts.iter().map(|account| account.id).max();
    let next_id = next.and_then(AccountId::next).unwrap_or(AccountId::DEFAULT);
    free.unwrap_or_else(|| AccountColor::for_id(next_id))
}

/// The folder the daemon proposes for a new account named `name`: `.claude-<name>` in the home
/// folder, which the default account's `.claude` shows, with a number added when another account
/// has it. `None` when the default account's folder is elsewhere, as with `CLAUDE_CONFIG_DIR`,
/// or relative, as from a home folder that is: the daemon then proposes none.
pub fn proposed_folder(state: &AppState, name: &str) -> Option<String> {
    let default = account(state, AccountId::DEFAULT)?;
    let text = default.folder.to_string_lossy();
    let style = PathStyle::of_text(&text);
    if style.file_name(&text) != Some(CLAUDE_FOLDER) || !style.is_absolute(&text) {
        return None;
    }
    let home = style.parent(&text)?;
    let folder_name = account_folder_name(name);
    let taken = |folder: &str| {
        let mut folders = state.accounts.iter();
        folders.any(|account| style.same_path(&account.folder.to_string_lossy(), folder))
    };
    let mut folder = style.join(home, &folder_name);
    let mut number: u32 = 2;
    while taken(&folder) {
        folder = style.join(home, &format!("{folder_name}{FOLDER_SEPARATOR}{number}"));
        number = number.saturating_add(1);
    }
    Some(style.shown(&folder).into_owned())
}

/// Keeps the typed name. Returns the folder field's new text while the folder follows the name.
pub fn type_account_name(state: &AppState, drafts: &mut Drafts, name: String) -> Option<String> {
    let draft = &mut drafts.add_account;
    draft.name = name;
    if draft.folder_typed {
        return None;
    }
    let folder = proposed_folder(state, &draft.name).unwrap_or_default();
    draft.folder.clone_from(&folder);
    Some(folder)
}

pub fn type_account_folder(drafts: &mut Drafts, folder: String) {
    let draft = &mut drafts.add_account;
    draft.folder = folder;
    draft.folder_typed = true;
}

pub fn with_add_form(state: &mut AppState, change: impl FnOnce(&mut AddAccountForm)) {
    if let Some(Dialog::AddAccount(form)) = &mut state.ui.dialog {
        change(form);
    }
}

/// Asks the daemon to add the account. A folder left as proposed goes as none, so the daemon
/// picks a free one.
pub fn add_account(state: &mut AppState, drafts: &Drafts) -> Vec<Outgoing> {
    let Some(Dialog::AddAccount(form)) = &mut state.ui.dialog else {
        return Vec::new();
    };
    let draft = &drafts.add_account;
    let name = draft.name.trim();
    if name.is_empty() || form.adding || form.added.is_some() {
        return Vec::new();
    }
    let folder = draft.folder.trim();
    let folder = (draft.folder_typed && !folder.is_empty()).then(|| PathBuf::from(folder));
    form.adding = true;
    form.error = None;
    vec![Outgoing {
        request: Request::AccountsAdd(AccountAddParams {
            name: name.to_owned(),
            folder,
            use_environment_api_key: form.use_api_key,
        }),
        purpose: Purpose::AccountAdded { color: form.color },
    }]
}

/// Leaves "Add an account" for the "Accounts" dialog, on the account just added.
pub fn close_add_account(state: &mut AppState) {
    let added = match &state.ui.dialog {
        Some(Dialog::AddAccount(form)) => form.added,
        _ => None,
    };
    open_accounts(state, added);
}

/// Keeps what the daemon says of an account, and the account itself, which the
/// `AccountsChanged` event also brings.
fn keep_state(state: &mut AppState, account_state: AccountState) {
    let account = account_state.account.clone();
    match state.accounts.iter_mut().find(|kept| kept.id == account.id) {
        Some(kept) => *kept = account,
        None => state.accounts.push(account),
    }
    state
        .sign_ins
        .states
        .insert(account_state.account.id, account_state);
}

/// An answer to `accounts.list`.
pub fn listed(state: &mut AppState, result: Result<ResponseBody, String>) {
    let sign_ins = &mut state.sign_ins;
    sign_ins.checking = false;
    sign_ins.checked_at = (!sign_ins.ask_again).then(SystemTime::now);
    sign_ins.ask_again = false;
    match result {
        Ok(ResponseBody::Accounts {
            accounts,
            environment_api_key,
        }) => {
            sign_ins.environment_api_key = environment_api_key;
            sign_ins.states = accounts
                .into_iter()
                .map(|account| (account.account.id, account))
                .collect();
        }
        Ok(other) => state
            .ui
            .report(format!("The accounts did not load: unexpected {other:?}")),
        Err(error) => state
            .ui
            .report(format!("The accounts did not load: {error}")),
    }
}

/// An answer to `accounts.add`: the dialog shows how to sign in to the account, which keeps the
/// color picked.
pub fn added(state: &mut AppState, color: AccountColor, result: Result<ResponseBody, String>) {
    let outcome = match result {
        Ok(ResponseBody::Account(account_state)) => {
            let id = account_state.account.id;
            set_color(state, id, color);
            keep_state(state, account_state);
            Ok(id)
        }
        Ok(other) => Err(format!("unexpected {other:?}")),
        Err(error) => Err(error),
    };
    with_add_form(state, |form| {
        form.adding = false;
        match outcome {
            Ok(id) => form.added = Some(id),
            Err(error) => form.error = Some(error),
        }
    });
}

/// An answer to `accounts.change`.
pub fn changed(state: &mut AppState, result: Result<ResponseBody, String>) {
    match result {
        Ok(ResponseBody::Account(account_state)) => keep_state(state, account_state),
        Ok(other) => state
            .ui
            .report(format!("The account did not change: unexpected {other:?}")),
        Err(error) => state
            .ui
            .report(format!("The account did not change: {error}")),
    }
}

#[cfg(test)]
mod tests {
    use cockpit_protocol::{
        ClaudeSessionId, CostUsd, Event, EventRecord, PastSession, RateLimitStatus, Seq, Timestamp,
        TokenUsage, TranscriptItem, TurnEnd, WindowUsage,
    };

    use super::*;
    use crate::state::actions;
    use crate::state::app_state::Loadable;
    use crate::state::{state_with, summary};

    fn work() -> AccountId {
        AccountId::new(1)
    }

    fn lab() -> AccountId {
        AccountId::new(2)
    }

    fn named(id: AccountId, name: &str, folder: &str) -> Account {
        Account {
            id,
            name: name.to_owned(),
            folder: PathBuf::from(folder),
            use_environment_api_key: false,
        }
    }

    /// Three accounts: the default "Personal", "Work", and "Lab", which uses the API key.
    fn three_accounts(state: &mut AppState) {
        state.accounts = vec![
            named(AccountId::DEFAULT, "Personal", "/home/sam/.claude"),
            named(work(), "Work", "/home/sam/.claude-work"),
            Account {
                use_environment_api_key: true,
                ..named(lab(), "Lab", "/home/sam/.claude-lab")
            },
        ];
    }

    fn in_folder(id: u32, folder: &str, account: AccountId) -> cockpit_protocol::AgentSummary {
        let mut summary = summary(id, AgentStatus::Idle);
        summary.folder = PathBuf::from(folder);
        summary.account = account;
        summary
    }

    fn usage(five_hour: f64, seven_day: f64) -> PlanUsage {
        let window = |utilization| WindowUsage {
            utilization: Some(utilization),
            resets_at: None,
            status: RateLimitStatus::Allowed,
        };
        let mut usage = PlanUsage::default();
        usage
            .windows
            .insert(UsageWindow::FiveHour, window(five_hour));
        usage
            .windows
            .insert(UsageWindow::SevenDay, window(seven_day));
        usage
    }

    fn signed(state: &mut AppState, id: AccountId, sign_in: SignIn) {
        let account = account(state, id).cloned().unwrap();
        state.sign_ins.states.insert(
            id,
            AccountState {
                account,
                sign_in,
                sign_in_command: "claude auth login".to_owned(),
            },
        );
    }

    #[test]
    fn labels_show_only_from_two_accounts() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        let none = label(&state, AccountId::DEFAULT);
        state.accounts = vec![named(AccountId::DEFAULT, "Default", "/home/sam/.claude")];
        let one = label(&state, AccountId::DEFAULT);
        let named_alone = label_of(&state, AccountId::DEFAULT).map(|label| label.name);
        three_accounts(&mut state);

        assert_eq!(none, None);
        assert_eq!(one, None);
        assert_eq!(named_alone.as_deref(), Some("Default"));
        assert_eq!(
            label(&state, AccountId::DEFAULT),
            Some(AccountLabel {
                id: AccountId::DEFAULT,
                name: "Personal".to_owned(),
                color: AccountColor::Blue,
            })
        );
        assert_eq!(
            label(&state, work()).map(|label| label.color),
            Some(AccountColor::Violet)
        );
        assert_eq!(label(&state, AccountId::new(9)), None);
    }

    #[test]
    fn a_new_agent_takes_the_picked_account_else_the_folders_last_else_the_default() {
        let mut older = in_folder(1, "/work/parser", lab());
        older.started_at = Timestamp::from_unix_millis(1_000);
        let mut newer = in_folder(2, "/work/parser", work());
        newer.started_at = Timestamp::from_unix_millis(2_000);
        let mut state = state_with(vec![older, newer]);
        three_accounts(&mut state);
        state.sessions = Loadable::Loaded(vec![PastSession {
            id: ClaudeSessionId::new("past".to_owned()),
            title: "Old work".to_owned(),
            folder: PathBuf::from("/work/notes"),
            last_activity: Timestamp::from_unix_millis(500),
            turns: 3,
            account: lab(),
        }]);
        let auto = NewAgentForm::default();
        let picked = NewAgentForm {
            account: Some(AccountId::DEFAULT),
            ..NewAgentForm::default()
        };
        let removed = NewAgentForm {
            account: Some(AccountId::new(7)),
            ..NewAgentForm::default()
        };

        assert_eq!(new_agent_account(&state, &auto, "/work/parser"), work());
        assert_eq!(new_agent_account(&state, &auto, "/work/parser/"), work());
        assert_eq!(new_agent_account(&state, &auto, "/work/notes"), lab());
        assert_eq!(
            new_agent_account(&state, &auto, "/work/new"),
            AccountId::DEFAULT
        );
        assert_eq!(new_agent_account(&state, &auto, ""), AccountId::DEFAULT);
        assert_eq!(
            new_agent_account(&state, &picked, "/work/parser"),
            AccountId::DEFAULT
        );
        assert_eq!(new_agent_account(&state, &removed, "/work/parser"), work());
        assert_eq!(
            picker_title(&state, "/work/parser"),
            "Account · Work ran the last agent in this folder"
        );
        assert_eq!(
            picker_title(&state, "/work/new"),
            "Account · the default account for a new folder"
        );
    }

    #[test]
    fn the_picker_follows_the_folder_until_an_account_is_picked_and_again_after_a_folder() {
        let mut state = state_with(vec![in_folder(1, "/work/parser", work())]);
        three_accounts(&mut state);
        let mut drafts = Drafts::default();
        actions::open_new_agent(&mut state, &mut drafts);
        let form = |state: &AppState| match &state.ui.dialog {
            Some(Dialog::NewAgent(form)) => form.clone(),
            other => panic!("the new agent dialog, not {other:?}"),
        };

        let opened = new_agent_account(&state, &form(&state), &drafts.new_agent.folder);
        pick_new_agent_account(&mut state, lab());
        let picked = new_agent_account(&state, &form(&state), &drafts.new_agent.folder);
        follow_folder_account(&mut state);
        let followed = new_agent_account(&state, &form(&state), &drafts.new_agent.folder);
        pick_new_agent_account(&mut state, lab());
        let start = actions::start_agent(&mut state, &drafts);

        assert_eq!(drafts.new_agent.folder, "/work/parser");
        assert_eq!(opened, work());
        assert_eq!(picked, lab());
        assert_eq!(followed, work());
        let [
            Outgoing {
                request: Request::AgentStart(params),
                ..
            },
        ] = start.as_slice()
        else {
            panic!("one start request, not {start:?}");
        };
        assert_eq!(params.account, lab());
    }

    #[test]
    fn the_menu_shows_each_accounts_plan_its_api_key_or_its_missing_sign_in() {
        let mut state = state_with(Vec::new());
        three_accounts(&mut state);
        state.accounts.push(named(
            AccountId::new(3),
            "Client",
            "/home/sam/.claude-client",
        ));
        state.plan_usage = usage(0.42, 0.63);
        state.other_plan_usage.insert(work(), usage(0.81, 0.34));
        signed(&mut state, AccountId::new(3), SignIn::NotSignedIn);

        let details: Vec<ChoiceDetail> = choices(&state)
            .into_iter()
            .map(|choice| choice.detail)
            .collect();

        assert_eq!(
            details,
            [
                ChoiceDetail::Plan {
                    five_hour: Some(42),
                    seven_day: Some(63)
                },
                ChoiceDetail::Plan {
                    five_hour: Some(81),
                    seven_day: Some(34)
                },
                ChoiceDetail::ApiKey,
                ChoiceDetail::NotSignedIn,
            ]
        );
    }

    #[test]
    fn the_plan_chip_shows_the_agents_account() {
        let mut state = state_with(Vec::new());
        three_accounts(&mut state);
        state.plan_usage = usage(0.42, 0.63);
        state.other_plan_usage.insert(work(), usage(0.81, 0.34));
        let personal = plan_chip(&state, AccountId::DEFAULT);
        let job = plan_chip(&state, work());
        let keyed = plan_chip(&state, lab());
        signed(&mut state, work(), SignIn::NotSignedIn);
        let signed_out = plan_chip(&state, work());
        signed(&mut state, work(), SignIn::Unknown);
        let unknown = plan_chip(&state, work());
        state.other_plan_usage.clear();
        let unreported = plan_chip(&state, work());

        assert_eq!(personal, PlanChip::Percent(Some(42)));
        assert_eq!(job, PlanChip::Percent(Some(81)));
        assert_eq!(keyed, PlanChip::ApiKey);
        assert_eq!(signed_out, PlanChip::NotSignedIn);
        assert_eq!(unknown, PlanChip::Percent(Some(81)));
        assert_eq!(unreported, PlanChip::Percent(None));
        assert_eq!(
            plan_card_accounts(&state, lab()),
            [lab(), AccountId::DEFAULT, work()]
        );
        assert_eq!(
            plan_card_accounts(&state, AccountId::DEFAULT),
            [AccountId::DEFAULT, work(), lab()]
        );
    }

    #[test]
    fn the_resume_list_shows_every_account_or_the_one_picked() {
        let mut state = state_with(Vec::new());
        three_accounts(&mut state);
        let past = |id: &str, account: AccountId| PastSession {
            id: ClaudeSessionId::new(id.to_owned()),
            title: id.to_owned(),
            folder: PathBuf::from("/work/app"),
            last_activity: Timestamp::from_unix_millis(1_000),
            turns: 2,
            account,
        };
        state.sessions = Loadable::Loaded(vec![
            past("mine", AccountId::DEFAULT),
            past("job", work()),
            past("bench", lab()),
        ]);
        let mut drafts = Drafts::default();
        let listed = |state: &AppState, drafts: &Drafts| -> Vec<String> {
            actions::resumable_sessions(state, "", drafts.new_agent.session_account)
                .iter()
                .map(|session| session.id.as_str().to_owned())
                .collect()
        };

        let all = listed(&state, &drafts);
        drafts.new_agent.session_highlighted = 2;
        toggle_session_account(&mut drafts, work());
        let only_work = listed(&state, &drafts);
        let highlight = drafts.new_agent.session_highlighted;
        toggle_session_account(&mut drafts, work());
        let again = listed(&state, &drafts);
        toggle_session_account(&mut drafts, lab());
        state.accounts.retain(|account| account.id != lab());
        let removed = listed(&state, &drafts);

        assert_eq!(all.len(), 3);
        assert_eq!(only_work, ["job"]);
        assert_eq!(highlight, 0);
        assert_eq!(again.len(), 3);
        assert_eq!(removed.len(), 3);
    }

    #[test]
    fn adding_an_account_proposes_its_folder_then_shows_how_to_sign_in() {
        let mut state = state_with(Vec::new());
        three_accounts(&mut state);
        let mut drafts = Drafts::default();
        open_add_account(&mut state, &mut drafts);
        let form = |state: &AppState| match &state.ui.dialog {
            Some(Dialog::AddAccount(form)) => form.clone(),
            other => panic!("the add dialog, not {other:?}"),
        };

        let color = form(&state).color;
        let proposed = type_account_name(&state, &mut drafts, "Client Work".to_owned());
        let request = add_account(&mut state, &drafts);
        let twice = add_account(&mut state, &drafts);
        let new = named(
            AccountId::new(3),
            "Client Work",
            "/home/sam/.claude-client-work",
        );
        added(
            &mut state,
            color,
            Ok(ResponseBody::Account(AccountState {
                account: new.clone(),
                sign_in: SignIn::NotSignedIn,
                sign_in_command:
                    "CLAUDE_CONFIG_DIR='/home/sam/.claude-client-work' claude auth login".to_owned(),
            })),
        );

        assert_eq!(color, AccountColor::Sand);
        assert_eq!(proposed.as_deref(), Some("/home/sam/.claude-client-work"));
        let [
            Outgoing {
                request: Request::AccountsAdd(params),
                purpose: Purpose::AccountAdded { color: sent },
            },
        ] = request.as_slice()
        else {
            panic!("one add request, not {request:?}");
        };
        assert_eq!(params.name, "Client Work");
        assert_eq!(params.folder, None);
        assert_eq!(*sent, AccountColor::Sand);
        assert_eq!(twice, []);
        assert_eq!(form(&state).added, Some(new.id));
        assert_eq!(color_of(&state, new.id), AccountColor::Sand);
        assert_eq!(state.sign_ins.sign_in(new.id), Some(SignIn::NotSignedIn));
        assert!(state.accounts.contains(&new));
        close_add_account(&mut state);
        assert_eq!(
            state.ui.dialog,
            Some(Dialog::Accounts(AccountsView {
                expanded: Some(new.id)
            }))
        );
    }

    fn color_of(state: &AppState, id: AccountId) -> AccountColor {
        color(state, id)
    }

    #[test]
    fn a_typed_folder_stays_and_goes_with_the_request_and_a_refusal_shows() {
        let mut state = state_with(Vec::new());
        three_accounts(&mut state);
        let mut drafts = Drafts::default();
        open_add_account(&mut state, &mut drafts);

        type_account_folder(&mut drafts, "/srv/claude/client".to_owned());
        let follows = type_account_name(&state, &mut drafts, "Client".to_owned());
        with_add_form(&mut state, |form| form.use_api_key = true);
        let request = add_account(&mut state, &drafts);
        added(
            &mut state,
            AccountColor::Pink,
            Err("an account is already named Client".to_owned()),
        );

        assert_eq!(follows, None);
        let [
            Outgoing {
                request: Request::AccountsAdd(params),
                ..
            },
        ] = request.as_slice()
        else {
            panic!("one add request, not {request:?}");
        };
        assert_eq!(params.folder, Some(PathBuf::from("/srv/claude/client")));
        assert!(params.use_environment_api_key);
        let Some(Dialog::AddAccount(form)) = &state.ui.dialog else {
            panic!("the add dialog");
        };
        assert!(!form.adding);
        assert_eq!(form.added, None);
        assert_eq!(
            form.error.as_deref(),
            Some("an account is already named Client")
        );
    }

    #[test]
    fn no_folder_is_proposed_when_the_default_account_is_elsewhere() {
        let mut state = state_with(Vec::new());
        state.accounts = vec![named(AccountId::DEFAULT, "Default", "/srv/claude-config")];
        let elsewhere = proposed_folder(&state, "Work");
        state.accounts = vec![named(AccountId::DEFAULT, "Default", "home/.claude")];
        let relative = proposed_folder(&state, "Work");

        assert_eq!(elsewhere, None);
        assert_eq!(relative, None);
    }

    #[test]
    fn the_proposed_folder_takes_a_number_as_the_daemon_does_when_another_account_has_it() {
        let mut state = state_with(Vec::new());
        three_accounts(&mut state);
        let unix = proposed_folder(&state, "Work");
        state.accounts = vec![
            named(AccountId::DEFAULT, "Default", r"C:\Users\sam\.claude"),
            named(work(), "Work", r"C:\Users\sam\.Claude-Work"),
            named(lab(), "Work 2", r"c:/users/sam/.claude-work-2"),
        ];
        let windows = proposed_folder(&state, "work");

        assert_eq!(unix.as_deref(), Some("/home/sam/.claude-work-2"));
        assert_eq!(windows.as_deref(), Some(r"C:\Users\sam\.claude-work-3"));
    }

    #[test]
    fn the_accounts_dialog_renames_changes_and_removes_only_a_free_account() {
        let mut state = state_with(vec![in_folder(1, "/work/app", work())]);
        three_accounts(&mut state);
        open_accounts(&mut state, None);
        toggle_account(&mut state, work());
        let opened = state.ui.dialog.clone();
        toggle_account(&mut state, work());
        let closed = state.ui.dialog.clone();

        let same_name = rename_account(&state, work(), " Work ");
        let empty = rename_account(&state, work(), "  ");
        let renamed = rename_account(&state, work(), "Job");
        let key = set_api_key(work(), true);
        set_color(&mut state, lab(), AccountColor::Green);

        assert_eq!(
            opened,
            Some(Dialog::Accounts(AccountsView {
                expanded: Some(work())
            }))
        );
        assert_eq!(
            closed,
            Some(Dialog::Accounts(AccountsView { expanded: None }))
        );
        assert_eq!(same_name, []);
        assert_eq!(empty, []);
        assert!(matches!(
            renamed.as_slice(),
            [Outgoing { request: Request::AccountsChange(AccountChangeParams { name: Some(name), use_environment_api_key: None, .. }), .. }] if name == "Job"
        ));
        assert!(matches!(
            key.as_slice(),
            [Outgoing {
                request: Request::AccountsChange(AccountChangeParams {
                    name: None,
                    use_environment_api_key: Some(true),
                    ..
                }),
                ..
            }]
        ));
        assert_eq!(color(&state, lab()), AccountColor::Green);
        assert_eq!(
            remove_block(&state, AccountId::DEFAULT),
            Some(RemoveBlock::Default)
        );
        assert_eq!(remove_block(&state, work()), Some(RemoveBlock::InUse(1)));
        assert_eq!(remove_block(&state, lab()), None);
        assert_eq!(remove_account(&mut state, work()), []);
        assert!(matches!(
            remove_account(&mut state, lab()).as_slice(),
            [Outgoing { request: Request::AccountsRemove(AccountParams { account }), .. }] if *account == lab()
        ));
    }

    #[test]
    fn the_sign_in_states_are_asked_once_then_again_on_check() {
        let mut state = state_with(Vec::new());
        let without_accounts = list_request(&mut state);
        three_accounts(&mut state);
        let first = list_request(&mut state);
        let while_asked = list_request(&mut state);
        listed(
            &mut state,
            Ok(ResponseBody::Accounts {
                accounts: Vec::new(),
                environment_api_key: true,
            }),
        );
        let after = list_request(&mut state);
        check_again(&mut state);
        let again = list_request(&mut state);

        assert_eq!(without_accounts, None);
        assert!(matches!(
            first,
            Some(Outgoing {
                request: Request::AccountsList,
                ..
            })
        ));
        assert_eq!(while_asked, None);
        assert!(state.sign_ins.environment_api_key);
        assert_eq!(after, None);
        assert!(again.is_some());
    }

    #[test]
    fn an_account_change_while_the_daemon_is_asked_asks_it_again() {
        let mut state = state_with(Vec::new());
        three_accounts(&mut state);
        let asked = list_request(&mut state);
        let accounts = state.accounts.clone();
        super::super::apply::apply(
            &mut state,
            &mut Drafts::default(),
            EventRecord {
                seq: Seq::new(1),
                event: Event::AccountsChanged { accounts },
            },
        );
        listed(
            &mut state,
            Ok(ResponseBody::Accounts {
                accounts: Vec::new(),
                environment_api_key: false,
            }),
        );
        let again = list_request(&mut state);
        listed(
            &mut state,
            Ok(ResponseBody::Accounts {
                accounts: Vec::new(),
                environment_api_key: false,
            }),
        );

        assert!(asked.is_some());
        assert!(again.is_some());
        assert_eq!(list_request(&mut state), None);
    }

    fn transcript_item(id: u32, body: ItemBody) -> TranscriptItem {
        TranscriptItem {
            id: ItemId::new(id),
            parent: None,
            body,
        }
    }

    fn signed_out_turn(not_signed_in: bool) -> ItemBody {
        ItemBody::TurnEnd(TurnEnd {
            duration: std::time::Duration::from_millis(900),
            cost: CostUsd::new(0.0),
            usage: TokenUsage::default(),
            error: Some("The account is not signed in".to_owned()),
            stopped: false,
            not_signed_in,
        })
    }

    fn message(text: &str) -> ItemBody {
        ItemBody::UserMessage(UserMessage {
            text: text.to_owned(),
            images: Vec::new(),
            queued: false,
        })
    }

    #[test]
    fn a_turn_without_login_shows_its_card_until_a_message_goes_after_it() {
        let mut waiting = in_folder(1, "/work/app", work());
        waiting.waits_for_sign_in = true;
        waiting.status = AgentStatus::NeedsYou;
        let mut state = state_with(vec![waiting.clone()]);
        three_accounts(&mut state);
        let notice = ItemBody::Notice {
            text: "The account Work is not signed in.".to_owned(),
            detail: None,
        };
        let entry = state.agents.get_mut(&AgentId::new(1)).unwrap();
        entry.transcript.merge_page(cockpit_protocol::HistoryPage {
            items: vec![
                transcript_item(1, message("Export the invoices")),
                transcript_item(2, notice),
                transcript_item(3, signed_out_turn(true)),
            ],
            has_older: false,
        });

        let card = sign_in_turn(state.agents.get(&AgentId::new(1)).unwrap());
        let line = waiting_line(&state, state.agents.get(&AgentId::new(1)).unwrap());
        let while_waiting = send_again(&mut state, AgentId::new(1));
        let entry = state.agents.get_mut(&AgentId::new(1)).unwrap();
        entry.summary.waits_for_sign_in = false;
        entry.summary.status = AgentStatus::Idle;
        let sent = send_again(&mut state, AgentId::new(1));
        let entry = state.agents.get_mut(&AgentId::new(1)).unwrap();
        entry
            .transcript
            .append(transcript_item(4, message("Export the invoices")));
        let after = sign_in_turn(entry);

        assert_eq!(
            card,
            Some(SignInTurn {
                turn_end: ItemId::new(3),
                notice: Some(ItemId::new(2)),
                message: Some(UserMessage {
                    text: "Export the invoices".to_owned(),
                    images: Vec::new(),
                    queued: false,
                }),
            })
        );
        assert_eq!(line.as_deref(), Some("Work is not signed in"));
        assert_eq!(turn_status(&waiting), AgentStatus::Idle);
        assert_eq!(while_waiting, []);
        assert!(matches!(
            sent.as_slice(),
            [Outgoing { request: Request::AgentPrompt(PromptParams { prompt, .. }), .. }] if prompt.text == "Export the invoices"
        ));
        assert_eq!(after, None);
    }

    #[test]
    fn the_message_goes_again_once_and_again_after_a_failed_send() {
        let mut state = state_with(vec![in_folder(1, "/work/app", work())]);
        three_accounts(&mut state);
        let agent = AgentId::new(1);
        let entry = state.agents.get_mut(&agent).unwrap();
        entry.transcript.merge_page(cockpit_protocol::HistoryPage {
            items: vec![
                transcript_item(1, message("Export the invoices")),
                transcript_item(2, signed_out_turn(true)),
            ],
            has_older: false,
        });
        let mut drafts = Drafts::default();

        let first = send_again(&mut state, agent);
        let second = send_again(&mut state, agent);
        let [Outgoing { purpose, .. }] = first.as_slice() else {
            panic!("one prompt, not {first:?}");
        };
        super::super::requests::apply_response(
            &mut state,
            &mut drafts,
            cockpit_protocol::Method::AgentPrompt,
            purpose.clone(),
            Err(cockpit_client::RequestError::Disconnected),
        );
        let after_failure = send_again(&mut state, agent);

        assert_eq!(second, []);
        assert_eq!(after_failure, first);
    }

    #[test]
    fn a_later_turn_with_a_login_shows_no_card() {
        let mut state = state_with(vec![in_folder(1, "/work/app", work())]);
        let entry = state.agents.get_mut(&AgentId::new(1)).unwrap();
        entry.transcript.merge_page(cockpit_protocol::HistoryPage {
            items: vec![
                transcript_item(1, message("Export")),
                transcript_item(2, signed_out_turn(true)),
                transcript_item(3, message("Export")),
                transcript_item(4, signed_out_turn(false)),
            ],
            has_older: false,
        });

        assert_eq!(
            sign_in_turn(state.agents.get(&AgentId::new(1)).unwrap()),
            None
        );
        assert_eq!(
            waiting_line(&state, state.agents.get(&AgentId::new(1)).unwrap()),
            None
        );
    }

    #[test]
    fn the_notification_of_an_agent_without_login_names_its_account() {
        let mut state = state_with(vec![in_folder(1, "/work/app", work())]);
        three_accounts(&mut state);
        let mut drafts = Drafts::default();
        let mut waiting = in_folder(1, "/work/app", work());
        waiting.status = AgentStatus::NeedsYou;
        waiting.waits_for_sign_in = true;

        let alert = super::super::apply::apply(
            &mut state,
            &mut drafts,
            EventRecord {
                seq: Seq::new(1),
                event: Event::AgentUpdated(waiting),
            },
        );

        assert_eq!(
            alert.map(|alert| alert.body).as_deref(),
            Some("The account Work is not signed in.")
        );
    }
}
