//! The Claude accounts in the window: their colors and labels, the new agent dialog's account,
//! the plan chip and card, the resume list's filter, an agent that waits for a sign-in, and the
//! "Add an account" and "Accounts" dialogs.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::SystemTime;

use cockpit_daemon::CLAUDE_HOME_FOLDER;
use cockpit_protocol::{
    Account, AccountAddParams, AccountChangeParams, AccountId, AccountParams, AccountState,
    AgentId, AgentStatus, AgentSummary, ClaudeRoute, CredentialSource, EnvironmentCredentials,
    FOLDER_SEPARATOR, ItemBody, ItemId, PlanUsage, Prompt, PromptDelivery, PromptParams, Request,
    ResponseBody, SignIn, SignInWait, TranscriptItem, TurnOutcome, UsageWindow, UserMessage,
    account_folder_name,
};

use super::app_state::{AgentEntry, AppState, MachineState};
use super::drafts::Drafts;
use super::requests::{Outgoing, Purpose};
use super::ui_state::{Dialog, NewAgentForm};
use crate::format;
use crate::machine::MachineId;
use crate::path_style::PathStyle;
use crate::prefs::AccountColor;

/// What `accounts.list` last answered.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SignIns {
    pub states: BTreeMap<AccountId, AccountState>,
    /// Whether the daemon's environment has `ANTHROPIC_API_KEY` or `ANTHROPIC_AUTH_TOKEN`.
    pub environment_credentials: EnvironmentCredentials,
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

    /// Where the account's agents send their requests, as the daemon last answered.
    pub fn route(&self, account: AccountId) -> Option<ClaudeRoute> {
        self.states.get(&account).map(|state| state.route)
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

/// What the app knows of `machine`, whose daemon has the accounts. Each machine numbers its own
/// accounts, so an account id means something only with its machine.
fn known(state: &AppState, machine: MachineId) -> &MachineState {
    state.machines.get(machine).unwrap_or(&state.machines.this)
}

/// Labels show once there is a choice: two accounts or more on the machine.
pub fn shows_labels(state: &AppState, machine: MachineId) -> bool {
    known(state, machine).accounts.len() >= 2
}

pub fn account(state: &AppState, machine: MachineId, id: AccountId) -> Option<&Account> {
    known(state, machine)
        .accounts
        .iter()
        .find(|account| account.id == id)
}

pub fn color(state: &AppState, machine: MachineId, id: AccountId) -> AccountColor {
    state
        .prefs
        .accounts
        .color(&known(state, machine).daemon.machine, id)
}

/// The account's label, whatever the number of accounts.
pub fn label_of(state: &AppState, machine: MachineId, id: AccountId) -> Option<AccountLabel> {
    let account = account(state, machine, id)?;
    Some(AccountLabel {
        id,
        name: account.name.clone(),
        color: color(state, machine, id),
    })
}

/// The label an agent, a folder or a session shows: `None` with fewer than two accounts.
pub fn label(state: &AppState, machine: MachineId, id: AccountId) -> Option<AccountLabel> {
    shows_labels(state, machine)
        .then(|| label_of(state, machine, id))
        .flatten()
}

/// Every account's label, the default one first.
pub fn labels(state: &AppState, machine: MachineId) -> Vec<AccountLabel> {
    known(state, machine)
        .accounts
        .iter()
        .filter_map(|account| label_of(state, machine, account.id))
        .collect()
}

/// How an account's agents pay for Claude.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Billing {
    /// A Claude subscription's plan, with its windows. Also an account whose sign-in the daemon
    /// cannot tell, or has not told yet.
    Plan,
    /// The daemon's `ANTHROPIC_API_KEY` or `ANTHROPIC_AUTH_TOKEN`: no plan limits.
    Environment,
    NotSignedIn,
    /// Another provider, such as Amazon Bedrock, which the account's settings choose: it needs no
    /// Claude login, and has no plan limits.
    Provider(ClaudeRoute),
}

pub fn billing(state: &AppState, machine: MachineId, id: AccountId) -> Billing {
    let sign_ins = &known(state, machine).sign_ins;
    if let Some(route) = sign_ins
        .route(id)
        .filter(|route| *route != ClaudeRoute::Anthropic)
    {
        return Billing::Provider(route);
    }
    if account(state, machine, id)
        .is_some_and(|account| account.credentials == CredentialSource::Environment)
    {
        return Billing::Environment;
    }
    match sign_ins.sign_in(id) {
        Some(SignIn::NotSignedIn) => Billing::NotSignedIn,
        Some(SignIn::SignedIn | SignIn::Unknown) | None => Billing::Plan,
    }
}

/// The account's plan usage: the default account's comes apart from the others'.
pub fn plan_usage(state: &AppState, machine: MachineId, id: AccountId) -> Option<&PlanUsage> {
    let known = known(state, machine);
    if id.is_default() {
        return Some(&known.plan_usage);
    }
    known.other_plan_usage.get(&id)
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
    Environment,
    NotSignedIn,
    Provider(ClaudeRoute),
}

pub fn plan_chip(state: &AppState, machine: MachineId, id: AccountId) -> PlanChip {
    match billing(state, machine, id) {
        Billing::Environment => PlanChip::Environment,
        Billing::NotSignedIn => PlanChip::NotSignedIn,
        Billing::Provider(route) => PlanChip::Provider(route),
        Billing::Plan => PlanChip::Percent(plan_usage(state, machine, id).and_then(plan_percent)),
    }
}

/// The plan card's accounts: the agent's first, then the others in the daemon's order.
pub fn plan_card_accounts(
    state: &AppState,
    machine: MachineId,
    agent_account: AccountId,
) -> Vec<AccountId> {
    let accounts = &known(state, machine).accounts;
    let mut ids: Vec<AccountId> = Vec::with_capacity(accounts.len());
    if account(state, machine, agent_account).is_some() {
        ids.push(agent_account);
    }
    let others = accounts
        .iter()
        .map(|account| account.id)
        .filter(|&id| id != agent_account);
    ids.extend(others);
    ids
}

/// The account of the agent started last on `machine`.
fn last_started_account(state: &AppState, machine: MachineId) -> Option<AccountId> {
    state
        .agents
        .values()
        .filter(|entry| MachineId::of(entry.id()) == machine)
        .max_by_key(|entry| entry.summary.started_at)
        .map(|entry| entry.summary.account)
}

/// The account a new agent runs with: the one picked, else the last started agent's, else the
/// default one. An account the daemon no longer has counts as none. The folder plays no part.
pub fn new_agent_account(state: &AppState, form: &NewAgentForm) -> AccountId {
    let machine = form.machine;
    let has = |id: &AccountId| account(state, machine, *id).is_some();
    form.account
        .filter(has)
        .or_else(|| last_started_account(state, machine).filter(has))
        .unwrap_or(AccountId::DEFAULT)
}

/// The account menu's title: why the button shows the account it shows.
pub fn picker_title(state: &AppState, machine: MachineId) -> String {
    let last = last_started_account(state, machine).and_then(|id| account(state, machine, id));
    match last {
        Some(last) => format!("Account · {} ran the last agent started", last.name),
        None => "Account · the default account".to_owned(),
    }
}

/// What the account menu says under an account's name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChoiceDetail {
    Plan {
        five_hour: Option<u8>,
        seven_day: Option<u8>,
    },
    Environment,
    NotSignedIn,
    Provider(ClaudeRoute),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountChoice {
    pub label: AccountLabel,
    pub detail: ChoiceDetail,
}

pub fn choices(state: &AppState, machine: MachineId) -> Vec<AccountChoice> {
    let all = labels(state, machine).into_iter();
    let choices = all.map(|label| {
        let detail = match billing(state, machine, label.id) {
            Billing::Environment => ChoiceDetail::Environment,
            Billing::NotSignedIn => ChoiceDetail::NotSignedIn,
            Billing::Provider(route) => ChoiceDetail::Provider(route),
            Billing::Plan => {
                let usage = plan_usage(state, machine, label.id);
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

/// The new agent dialog's account menu picks an account for as long as the dialog stays on its
/// machine. The resume list then shows that account's sessions, with the first one highlighted.
pub fn pick_new_agent_account(state: &mut AppState, drafts: &mut Drafts, id: AccountId) {
    if let Some(Dialog::NewAgent(form)) = &mut state.ui.dialog {
        form.account = Some(id);
        form.menu = None;
        drafts.new_agent.session_highlighted = 0;
    }
}

/// The account whose sessions the "Resume a session" tab lists: the dialog's account. `None` when
/// the machine has one account, and the tab then lists every session.
pub fn resume_account(state: &AppState) -> Option<AccountId> {
    let Some(Dialog::NewAgent(form)) = &state.ui.dialog else {
        return None;
    };
    shows_labels(state, form.machine).then(|| new_agent_account(state, form))
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
    let items: Vec<&TranscriptItem> = entry
        .transcript
        .items()
        .filter(|item| item.parent.is_none())
        .collect();
    let end = items
        .iter()
        .rposition(|item| matches!(item.body, ItemBody::TurnEnd(_) | ItemBody::UserMessage(_)))?;
    let turn_end = items.get(end)?;
    let ItemBody::TurnEnd(ended) = &turn_end.body else {
        return None;
    };
    if ended.outcome != TurnOutcome::NotSignedIn {
        return None;
    }
    let before = items.get(..end).unwrap_or_default();
    let notice = before
        .last()
        .filter(|item| matches!(item.body, ItemBody::Notice { .. }))
        .map(|item| item.id);
    let message = before.iter().rev().find_map(|item| match &item.body {
        ItemBody::UserMessage(message) => Some(message.clone()),
        _ => None,
    });
    Some(SignInTurn {
        turn_end: turn_end.id,
        notice,
        message,
    })
}

/// Whether Claude Code refused the API key or token the account takes from the daemon's
/// environment. With neither variable set, Claude Code used the folder's login instead.
pub fn credentials_refused(state: &AppState, machine: MachineId, id: AccountId) -> bool {
    known(state, machine).sign_ins.environment_credentials == EnvironmentCredentials::Set
        && account(state, machine, id)
            .is_some_and(|account| account.credentials == CredentialSource::Environment)
}

/// Whether "Send the message again" sends: the account signed in, a check found its login, or no
/// check can end the wait; the app holds the message, and the card has not sent it yet. No check
/// ends the wait when the daemon cannot see the sign-in (a login in the macOS Keychain), or when
/// the account's credentials come from the environment. A login the check finds from before the
/// failure does not end the wait, but it may well work again: a refusal can pass.
pub fn can_send_again(state: &AppState, entry: &AgentEntry, turn: &SignInTurn) -> bool {
    let id = entry.summary.account;
    let machine = MachineId::of(entry.id());
    let checked = known(state, machine).sign_ins.sign_in(id);
    let cannot_tell = checked == Some(SignIn::Unknown)
        || account(state, machine, id)
            .is_some_and(|account| account.credentials == CredentialSource::Environment);
    let signed_in = entry.summary.sign_in_wait == SignInWait::NotWaiting
        || cannot_tell
        || checked == Some(SignIn::SignedIn);
    signed_in && turn.message.is_some() && entry.sent_again != Some(turn.turn_end)
}

/// Whether the agent's sign-in card shows, and its "Send the message again" sends.
pub fn sends_again(state: &AppState, agent: AgentId) -> bool {
    let Some(entry) = state.agents.get(&agent) else {
        return false;
    };
    sign_in_turn(entry).is_some_and(|turn| can_send_again(state, entry, &turn))
}

/// Sends the message of the turn that found no login again, once.
pub fn send_again(state: &mut AppState, agent: AgentId) -> Vec<Outgoing> {
    let Some(entry) = state.agents.get(&agent) else {
        return Vec::new();
    };
    let Some(turn) = sign_in_turn(entry).filter(|turn| can_send_again(state, entry, turn)) else {
        return Vec::new();
    };
    let Some(message) = turn.message else {
        return Vec::new();
    };
    if let Some(entry) = state.agents.get_mut(&agent) {
        entry.sent_again = Some(turn.turn_end);
    }
    state.ui.chat_scrolled_up.remove(&agent);
    state.ui.chat_jumps = state.ui.chat_jumps.wrapping_add(1);
    let prompt = Prompt {
        text: message.text,
        images: message.images,
    };
    vec![Outgoing {
        request: Request::AgentPrompt(PromptParams {
            agent,
            prompt,
            delivery: PromptDelivery::Steer,
        }),
        purpose: Purpose::SendAgain {
            agent,
            turn_end: turn.turn_end,
        },
        to: MachineId::of(agent),
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
    let only_sign_in = summary.sign_in_wait == SignInWait::Waiting && summary.pending_count == 0;
    if only_sign_in && summary.status == AgentStatus::NeedsYou {
        return AgentStatus::Idle;
    }
    summary.status
}

/// The sidebar's second line for an agent whose turn found no login.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignInLine {
    /// The agent waits for its account's sign-in: "Work is not signed in".
    NotSignedIn(String),
    /// The account signed in, and the message waits to go again.
    SendAgain,
}

impl SignInLine {
    pub fn text(&self) -> &str {
        match self {
            Self::NotSignedIn(text) => text,
            Self::SendAgain => "Signed in: send the message again",
        }
    }
}

/// The sidebar's line for an agent whose turn found no login, until its message goes again.
pub fn sign_in_line(state: &AppState, entry: &AgentEntry) -> Option<SignInLine> {
    if entry.summary.sign_in_wait == SignInWait::NotWaiting {
        return sends_again(state, entry.id()).then_some(SignInLine::SendAgain);
    }
    let line = match account(state, MachineId::of(entry.id()), entry.summary.account) {
        Some(account) => format!("{} is not signed in", account.name),
        None => "Not signed in".to_owned(),
    };
    Some(SignInLine::NotSignedIn(line))
}

/// How many agents besides `agent`, on its machine, wait for the same account's sign-in.
pub fn others_waiting(state: &AppState, agent: AgentId, id: AccountId) -> usize {
    let machine = MachineId::of(agent);
    let others = state.agents.values().filter(|entry| {
        entry.id() != agent
            && MachineId::of(entry.id()) == machine
            && entry.summary.account == id
            && entry.summary.sign_in_wait == SignInWait::Waiting
    });
    others.count()
}

/// The agents of an account of `machine`, running or not.
pub fn agent_count(state: &AppState, machine: MachineId, id: AccountId) -> usize {
    let agents = state.agents.values();
    agents
        .filter(|entry| MachineId::of(entry.id()) == machine && entry.summary.account == id)
        .count()
}

/// `accounts.list` to `machine`, when the window needs its sign-in states and they are not on
/// their way.
pub fn list_request(state: &mut AppState, machine: MachineId) -> Option<Outgoing> {
    let known = state.machines.get_mut(machine)?;
    let sign_ins = &mut known.sign_ins;
    if known.accounts.is_empty() || sign_ins.checked_at.is_some() || sign_ins.checking {
        return None;
    }
    sign_ins.checking = true;
    Some(Outgoing {
        request: Request::AccountsList,
        purpose: Purpose::Accounts,
        to: machine,
    })
}

/// "Check again": the daemon looks at the accounts' folders, and ends the waits of the agents
/// whose account signed in since.
pub fn check_again(state: &mut AppState, machine: MachineId) {
    if let Some(known) = state.machines.get_mut(machine) {
        known.sign_ins.refresh();
    }
}

/// The "Accounts" dialog: which machine's accounts it shows, and which card is open.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AccountsView {
    pub machine: MachineId,
    pub expanded: Option<AccountId>,
    /// The new agent dialog it was opened from, which closing it goes back to.
    pub from_new_agent: Option<NewAgentForm>,
}

/// Opens the "Accounts" dialog. Opened from the new agent dialog, or from "Add an account" that
/// came from there, it keeps that dialog to go back to.
///
/// It shows the accounts of the machine the dialog it comes from is about, else the selected
/// agent's.
pub fn open_accounts(state: &mut AppState, expanded: Option<AccountId>) {
    let selected = state.selected_machine();
    let (machine, from_new_agent) = match state.ui.dialog.take() {
        Some(Dialog::NewAgent(form)) => (form.machine, Some(NewAgentForm { menu: None, ..form })),
        Some(Dialog::Accounts(view)) => (view.machine, view.from_new_agent),
        Some(Dialog::AddAccount(form)) => (form.machine, form.from_new_agent),
        Some(_) | None => (selected, None),
    };
    state.ui.popover = None;
    state.ui.dialog = Some(Dialog::Accounts(AccountsView {
        machine,
        expanded,
        from_new_agent,
    }));
    check_again(state, machine);
}

/// Shows another machine's accounts in the "Accounts" dialog.
pub fn pick_accounts_machine(state: &mut AppState, machine: MachineId) {
    if let Some(Dialog::Accounts(view)) = &mut state.ui.dialog
        && view.machine != machine
    {
        view.machine = machine;
        view.expanded = None;
        check_again(state, machine);
    }
}

pub fn toggle_account(state: &mut AppState, id: AccountId) {
    if let Some(Dialog::Accounts(view)) = &mut state.ui.dialog {
        view.expanded = (view.expanded != Some(id)).then_some(id);
    }
}

pub fn set_color(state: &mut AppState, machine: MachineId, id: AccountId, color: AccountColor) {
    let name = known(state, machine).daemon.machine.clone();
    state.prefs.accounts.set_color(&name, id, color);
}

/// Renames the account, when the name changed and is not empty.
pub fn rename_account(
    state: &AppState,
    machine: MachineId,
    id: AccountId,
    name: &str,
) -> Vec<Outgoing> {
    let name = name.trim();
    let unchanged = account(state, machine, id).is_none_or(|account| account.name == name);
    if name.is_empty() || unchanged {
        return Vec::new();
    }
    change_request(
        machine,
        AccountChangeParams {
            account: id,
            name: Some(name.to_owned()),
            credentials: None,
        },
    )
}

pub fn set_environment_credentials(machine: MachineId, id: AccountId, uses: bool) -> Vec<Outgoing> {
    change_request(
        machine,
        AccountChangeParams {
            account: id,
            name: None,
            credentials: Some(credentials(uses)),
        },
    )
}

fn credentials(uses_environment: bool) -> CredentialSource {
    if uses_environment {
        CredentialSource::Environment
    } else {
        CredentialSource::SignIn
    }
}

fn change_request(machine: MachineId, params: AccountChangeParams) -> Vec<Outgoing> {
    vec![Outgoing {
        request: Request::AccountsChange(params),
        purpose: Purpose::AccountChanged,
        to: machine,
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

pub fn remove_block(state: &AppState, machine: MachineId, id: AccountId) -> Option<RemoveBlock> {
    if id.is_default() {
        return Some(RemoveBlock::Default);
    }
    let count = agent_count(state, machine, id);
    (count > 0).then_some(RemoveBlock::InUse(count))
}

pub fn remove_account(state: &mut AppState, machine: MachineId, id: AccountId) -> Vec<Outgoing> {
    if remove_block(state, machine, id).is_some() {
        return Vec::new();
    }
    if let Some(Dialog::Accounts(view)) = &mut state.ui.dialog {
        view.expanded.take_if(|expanded| *expanded == id);
    }
    vec![Outgoing {
        request: Request::AccountsRemove(AccountParams { account: id }),
        purpose: Purpose::Action,
        to: machine,
    }]
}

/// The "Add an account" dialog. Its text fields are in the drafts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddAccountForm {
    /// The machine whose daemon adds the account.
    pub machine: MachineId,
    pub color: AccountColor,
    pub use_environment: bool,
    /// The `accounts.add` request is on its way.
    pub adding: bool,
    /// The account, once the daemon added it: the dialog then shows how to sign in to it.
    pub added: Option<AccountId>,
    /// Why the daemon refused the account.
    pub error: Option<String>,
    /// The new agent dialog that the "Accounts" dialog it came from goes back to.
    pub from_new_agent: Option<NewAgentForm>,
}

pub fn open_add_account(state: &mut AppState, drafts: &mut Drafts) {
    let selected = state.selected_machine();
    let (machine, from_new_agent) = match state.ui.dialog.take() {
        Some(Dialog::Accounts(view)) => (view.machine, view.from_new_agent),
        Some(_) | None => (selected, None),
    };
    state.ui.popover = None;
    state.ui.dialog = Some(Dialog::AddAccount(AddAccountForm {
        machine,
        color: free_color(state, machine),
        use_environment: false,
        adding: false,
        added: None,
        error: None,
        from_new_agent,
    }));
    drafts.add_account = Default::default();
}

/// The first color no account has, else the one the next id would take.
pub fn free_color(state: &AppState, machine: MachineId) -> AccountColor {
    use strum::VariantArray;

    let accounts = &known(state, machine).accounts;
    let taken: Vec<AccountColor> = accounts
        .iter()
        .map(|account| color(state, machine, account.id))
        .collect();
    let free = AccountColor::VARIANTS
        .iter()
        .copied()
        .find(|color| !taken.contains(color));
    let next = accounts.iter().map(|account| account.id).max();
    let next_id = next.and_then(AccountId::next).unwrap_or(AccountId::DEFAULT);
    free.unwrap_or_else(|| AccountColor::for_id(next_id))
}

/// The folder the daemon proposes for a new account named `name`: `.claude-<name>` in the home
/// folder, which the default account's `.claude` shows, with a number added when another account
/// has it. `None` when the default account's folder is elsewhere, as with `CLAUDE_CONFIG_DIR`,
/// or relative, as from a home folder that is: the daemon then proposes none.
pub fn proposed_folder(state: &AppState, machine: MachineId, name: &str) -> Option<String> {
    let default = account(state, machine, AccountId::DEFAULT)?;
    let text = default.folder.to_string_lossy();
    let style = PathStyle::of_text(&text);
    if style.file_name(&text) != Some(CLAUDE_HOME_FOLDER) || !style.is_absolute(&text) {
        return None;
    }
    let home = style.parent(&text)?;
    let folder_name = account_folder_name(name);
    let taken = |folder: &str| {
        let mut folders = known(state, machine).accounts.iter();
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
    let machine = match &state.ui.dialog {
        Some(Dialog::AddAccount(form)) => form.machine,
        _ => state.selected_machine(),
    };
    let folder = proposed_folder(state, machine, &draft.name).unwrap_or_default();
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
            credentials: credentials(form.use_environment),
        }),
        purpose: Purpose::AccountAdded { color: form.color },
        to: form.machine,
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
fn keep_state(state: &mut AppState, machine: MachineId, account_state: AccountState) {
    let Some(known) = state.machines.get_mut(machine) else {
        return;
    };
    let account = account_state.account.clone();
    match known.accounts.iter_mut().find(|kept| kept.id == account.id) {
        Some(kept) => *kept = account,
        None => known.accounts.push(account),
    }
    known
        .sign_ins
        .states
        .insert(account_state.account.id, account_state);
}

/// An answer to `accounts.list` from `machine`.
pub fn listed(state: &mut AppState, machine: MachineId, result: Result<ResponseBody, String>) {
    let Some(known) = state.machines.get_mut(machine) else {
        return;
    };
    let sign_ins = &mut known.sign_ins;
    sign_ins.checking = false;
    sign_ins.checked_at = (!sign_ins.ask_again).then(SystemTime::now);
    sign_ins.ask_again = false;
    match result {
        Ok(ResponseBody::Accounts {
            accounts,
            environment_credentials,
        }) => {
            sign_ins.environment_credentials = environment_credentials;
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
pub fn added(
    state: &mut AppState,
    machine: MachineId,
    color: AccountColor,
    result: Result<ResponseBody, String>,
) {
    let outcome = match result {
        Ok(ResponseBody::Account(account_state)) => {
            let id = account_state.account.id;
            set_color(state, machine, id, color);
            keep_state(state, machine, account_state);
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
pub fn changed(state: &mut AppState, machine: MachineId, result: Result<ResponseBody, String>) {
    match result {
        Ok(ResponseBody::Account(account_state)) => keep_state(state, machine, account_state),
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
    use std::time::Duration;

    use cockpit_client::RequestError;
    use cockpit_protocol::{
        ClaudeSessionId, CostUsd, Event, EventRecord, HistoryPage, Method, NoticeKind, OlderItems,
        PastSession, PermissionMode, RateLimitStatus, Seq, Timestamp, TokenUsage, TurnEnd,
        WindowUsage,
    };

    use super::*;
    use crate::daemon_label::DaemonLabel;
    use crate::machine::MachineId;
    use crate::state::app_state::Loadable;
    use crate::state::ui_state::FormMenu;
    use crate::state::{ConnectionState, actions, folder_field, state_with, summary};

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
            credentials: CredentialSource::SignIn,
        }
    }

    /// Three accounts: the default "Personal", "Work", and "Lab", which uses the environment's
    /// API key or token.
    fn three_accounts(state: &mut AppState) {
        state.machines.this.accounts = vec![
            named(AccountId::DEFAULT, "Personal", "/home/sam/.claude"),
            named(work(), "Work", "/home/sam/.claude-work"),
            Account {
                credentials: CredentialSource::Environment,
                ..named(lab(), "Lab", "/home/sam/.claude-lab")
            },
        ];
    }

    /// This computer with three accounts, and devbox, whose account 1 is "Build", connected.
    fn with_devbox(state: &mut AppState) -> MachineId {
        let devbox = MachineId::new(1);
        three_accounts(state);
        let mut known = MachineState::new(DaemonLabel::ssh("devbox".to_owned()));
        known.connection = ConnectionState::Connected;
        known.accounts = vec![
            named(AccountId::DEFAULT, "Default", "/home/dev/.claude"),
            named(work(), "Build", "/home/dev/.claude-build"),
        ];
        state.machines.others.insert(devbox, known);
        devbox
    }

    #[test]
    fn an_account_id_names_another_account_on_each_machine() {
        let mut state = state_with(Vec::new());
        let devbox = with_devbox(&mut state);

        let here = label_of(&state, MachineId::THIS, work()).map(|label| label.name);
        let there = label_of(&state, devbox, work()).map(|label| label.name);

        assert_eq!(here.as_deref(), Some("Work"));
        assert_eq!(there.as_deref(), Some("Build"));
        assert_eq!(labels(&state, devbox).len(), 2);
        assert!(label_of(&state, devbox, lab()).is_none());
    }

    #[test]
    fn the_accounts_dialog_shows_the_machine_picked_and_asks_its_sign_ins() {
        let mut state = state_with(Vec::new());
        let devbox = with_devbox(&mut state);
        state.machines.this.connection = ConnectionState::Connected;
        open_accounts(&mut state, Some(work()));

        pick_accounts_machine(&mut state, devbox);
        let asked = list_request(&mut state, devbox);

        assert!(matches!(
            &state.ui.dialog,
            Some(Dialog::Accounts(view)) if view.machine == devbox && view.expanded.is_none()
        ));
        assert_eq!(asked.map(|outgoing| outgoing.to), Some(devbox));
    }

    fn in_folder(id: u32, folder: &str, account: AccountId) -> AgentSummary {
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
        let account = account(state, MachineId::THIS, id).cloned().unwrap();
        state.machines.this.sign_ins.states.insert(
            id,
            AccountState {
                account,
                sign_in,
                sign_in_command: "claude auth login".to_owned(),
                route: ClaudeRoute::Anthropic,
            },
        );
    }

    #[test]
    fn labels_show_only_from_two_accounts() {
        let mut state = state_with(vec![summary(1, AgentStatus::Idle)]);
        let none = label(&state, MachineId::THIS, AccountId::DEFAULT);
        state.machines.this.accounts =
            vec![named(AccountId::DEFAULT, "Default", "/home/sam/.claude")];
        let one = label(&state, MachineId::THIS, AccountId::DEFAULT);
        let named_alone =
            label_of(&state, MachineId::THIS, AccountId::DEFAULT).map(|label| label.name);
        three_accounts(&mut state);

        assert_eq!(none, None);
        assert_eq!(one, None);
        assert_eq!(named_alone.as_deref(), Some("Default"));
        assert_eq!(
            label(&state, MachineId::THIS, AccountId::DEFAULT),
            Some(AccountLabel {
                id: AccountId::DEFAULT,
                name: "Personal".to_owned(),
                color: AccountColor::Blue,
            })
        );
        assert_eq!(
            label(&state, MachineId::THIS, work()).map(|label| label.color),
            Some(AccountColor::Violet)
        );
        assert_eq!(label(&state, MachineId::THIS, AccountId::new(9)), None);
    }

    #[test]
    fn a_new_agent_takes_the_picked_account_else_the_last_started_agents_else_the_default() {
        let mut older = in_folder(1, "/work/parser", work());
        older.started_at = Timestamp::from_unix_millis(1_000);
        let mut newer = in_folder(2, "/work/notes", lab());
        newer.started_at = Timestamp::from_unix_millis(2_000);
        let mut state = state_with(vec![older, newer]);
        three_accounts(&mut state);
        let mut no_agent = state_with(Vec::new());
        three_accounts(&mut no_agent);
        let auto = NewAgentForm::default();
        let picked = NewAgentForm {
            account: Some(AccountId::DEFAULT),
            ..NewAgentForm::default()
        };
        let removed = NewAgentForm {
            account: Some(AccountId::new(7)),
            ..NewAgentForm::default()
        };

        assert_eq!(new_agent_account(&state, &auto), lab());
        assert_eq!(new_agent_account(&state, &picked), AccountId::DEFAULT);
        assert_eq!(new_agent_account(&state, &removed), lab());
        assert_eq!(new_agent_account(&no_agent, &auto), AccountId::DEFAULT);
        assert_eq!(
            picker_title(&state, MachineId::THIS),
            "Account · Lab ran the last agent started"
        );
        assert_eq!(
            picker_title(&no_agent, MachineId::THIS),
            "Account · the default account"
        );
    }

    #[test]
    fn the_folder_never_changes_the_dialogs_account() {
        let mut state = state_with(vec![in_folder(1, "/work/parser", work())]);
        three_accounts(&mut state);
        state.machines.this.sessions = Loadable::Loaded(vec![PastSession {
            id: ClaudeSessionId::new("past".to_owned()),
            title: "Old work".to_owned(),
            folder: PathBuf::from("/work/notes"),
            last_activity: Timestamp::from_unix_millis(500),
            turns: 3,
            account: lab(),
        }]);
        let mut drafts = Drafts::default();
        actions::open_new_agent(&mut state, &mut drafts);
        let form = |state: &AppState| match &state.ui.dialog {
            Some(Dialog::NewAgent(form)) => form.clone(),
            other => panic!("the new agent dialog, not {other:?}"),
        };

        let opened = new_agent_account(&state, &form(&state));
        folder_field::set_text(&mut state, &mut drafts, "/work/notes".to_owned());
        let typed = new_agent_account(&state, &form(&state));
        pick_new_agent_account(&mut state, &mut drafts, AccountId::DEFAULT);
        folder_field::pick(&mut state, &mut drafts, "/work/parser".to_owned());
        let picked = new_agent_account(&state, &form(&state));
        let start = actions::start_agent(&mut state, &drafts);

        assert_eq!(opened, work());
        assert_eq!(typed, work());
        assert_eq!(picked, AccountId::DEFAULT);
        let [
            Outgoing {
                request: Request::AgentStart(params),
                ..
            },
        ] = start.as_slice()
        else {
            panic!("one start request, not {start:?}");
        };
        assert_eq!(params.account, AccountId::DEFAULT);
    }

    #[test]
    fn the_menu_shows_each_accounts_plan_its_api_key_or_its_missing_sign_in() {
        let mut state = state_with(Vec::new());
        three_accounts(&mut state);
        state.machines.this.accounts.push(named(
            AccountId::new(3),
            "Client",
            "/home/sam/.claude-client",
        ));
        state.machines.this.plan_usage = usage(0.42, 0.63);
        state
            .machines
            .this
            .other_plan_usage
            .insert(work(), usage(0.81, 0.34));
        signed(&mut state, AccountId::new(3), SignIn::NotSignedIn);

        let details: Vec<ChoiceDetail> = choices(&state, MachineId::THIS)
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
                ChoiceDetail::Environment,
                ChoiceDetail::NotSignedIn,
            ]
        );
    }

    #[test]
    fn an_account_on_another_provider_needs_no_login_and_has_no_plan() {
        let mut state = state_with(Vec::new());
        three_accounts(&mut state);
        signed(&mut state, work(), SignIn::NotSignedIn);
        if let Some(account) = state.machines.this.sign_ins.states.get_mut(&work()) {
            account.route = ClaudeRoute::Bedrock;
        }

        assert_eq!(
            billing(&state, MachineId::THIS, work()),
            Billing::Provider(ClaudeRoute::Bedrock)
        );
        assert_eq!(
            plan_chip(&state, MachineId::THIS, work()),
            PlanChip::Provider(ClaudeRoute::Bedrock)
        );
        assert!(choices(&state, MachineId::THIS).iter().any(|choice| {
            choice.label.id == work()
                && choice.detail == ChoiceDetail::Provider(ClaudeRoute::Bedrock)
        }));
    }

    #[test]
    fn the_plan_chip_shows_the_agents_account() {
        let mut state = state_with(Vec::new());
        three_accounts(&mut state);
        state.machines.this.plan_usage = usage(0.42, 0.63);
        state
            .machines
            .this
            .other_plan_usage
            .insert(work(), usage(0.81, 0.34));
        let personal = plan_chip(&state, MachineId::THIS, AccountId::DEFAULT);
        let job = plan_chip(&state, MachineId::THIS, work());
        let keyed = plan_chip(&state, MachineId::THIS, lab());
        signed(&mut state, work(), SignIn::NotSignedIn);
        let signed_out = plan_chip(&state, MachineId::THIS, work());
        signed(&mut state, work(), SignIn::Unknown);
        let unknown = plan_chip(&state, MachineId::THIS, work());
        state.machines.this.other_plan_usage.clear();
        let unreported = plan_chip(&state, MachineId::THIS, work());

        assert_eq!(personal, PlanChip::Percent(Some(42)));
        assert_eq!(job, PlanChip::Percent(Some(81)));
        assert_eq!(keyed, PlanChip::Environment);
        assert_eq!(signed_out, PlanChip::NotSignedIn);
        assert_eq!(unknown, PlanChip::Percent(Some(81)));
        assert_eq!(unreported, PlanChip::Percent(None));
        assert_eq!(
            plan_card_accounts(&state, MachineId::THIS, lab()),
            [lab(), AccountId::DEFAULT, work()]
        );
        assert_eq!(
            plan_card_accounts(&state, MachineId::THIS, AccountId::DEFAULT),
            [AccountId::DEFAULT, work(), lab()]
        );
    }

    #[test]
    fn the_resume_list_shows_the_dialogs_account_or_every_session_with_one_account() {
        let mut state = state_with(Vec::new());
        three_accounts(&mut state);
        let mut drafts = Drafts::default();
        actions::open_new_agent(&mut state, &mut drafts);
        let past = |id: &str, account: AccountId| PastSession {
            id: ClaudeSessionId::new(id.to_owned()),
            title: id.to_owned(),
            folder: PathBuf::from("/work/app"),
            last_activity: Timestamp::from_unix_millis(1_000),
            turns: 2,
            account,
        };
        state.machines.this.sessions = Loadable::Loaded(vec![
            past("mine", AccountId::DEFAULT),
            past("job", work()),
            past("bench", lab()),
        ]);
        let listed = |state: &AppState, drafts: &Drafts| -> Vec<String> {
            actions::listed_sessions(state, drafts)
                .iter()
                .map(|session| session.id.as_str().to_owned())
                .collect()
        };

        let default = listed(&state, &drafts);
        drafts.new_agent.session_highlighted = 2;
        pick_new_agent_account(&mut state, &mut drafts, work());
        let work_only = listed(&state, &drafts);
        let highlight = drafts.new_agent.session_highlighted;
        state
            .machines
            .this
            .accounts
            .retain(|account| account.id.is_default());
        let one_account = listed(&state, &drafts);

        assert_eq!(default, ["mine"]);
        assert_eq!(work_only, ["job"]);
        assert_eq!(highlight, 0);
        assert_eq!(one_account.len(), 3);
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
            MachineId::THIS,
            color,
            Ok(ResponseBody::Account(AccountState {
                account: new.clone(),
                sign_in: SignIn::NotSignedIn,
                sign_in_command:
                    "CLAUDE_CONFIG_DIR='/home/sam/.claude-client-work' claude auth login".to_owned(),
                route: ClaudeRoute::Anthropic,
            })),
        );

        assert_eq!(color, AccountColor::Sand);
        assert_eq!(proposed.as_deref(), Some("/home/sam/.claude-client-work"));
        let [
            Outgoing {
                request: Request::AccountsAdd(params),
                purpose: Purpose::AccountAdded { color: sent },
                to: MachineId::THIS,
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
        assert_eq!(
            state.machines.this.sign_ins.sign_in(new.id),
            Some(SignIn::NotSignedIn)
        );
        assert!(state.machines.this.accounts.contains(&new));
        close_add_account(&mut state);
        assert_eq!(
            state.ui.dialog,
            Some(Dialog::Accounts(AccountsView {
                machine: MachineId::THIS,
                expanded: Some(new.id),
                from_new_agent: None,
            }))
        );
    }

    fn color_of(state: &AppState, id: AccountId) -> AccountColor {
        color(state, MachineId::THIS, id)
    }

    #[test]
    fn a_typed_folder_stays_and_goes_with_the_request_and_a_refusal_shows() {
        let mut state = state_with(Vec::new());
        three_accounts(&mut state);
        let mut drafts = Drafts::default();
        open_add_account(&mut state, &mut drafts);

        type_account_folder(&mut drafts, "/srv/claude/client".to_owned());
        let follows = type_account_name(&state, &mut drafts, "Client".to_owned());
        with_add_form(&mut state, |form| form.use_environment = true);
        let request = add_account(&mut state, &drafts);
        added(
            &mut state,
            MachineId::THIS,
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
        assert_eq!(params.credentials, CredentialSource::Environment);
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
        state.machines.this.accounts =
            vec![named(AccountId::DEFAULT, "Default", "/srv/claude-config")];
        let elsewhere = proposed_folder(&state, MachineId::THIS, "Work");
        state.machines.this.accounts = vec![named(AccountId::DEFAULT, "Default", "home/.claude")];
        let relative = proposed_folder(&state, MachineId::THIS, "Work");

        assert_eq!(elsewhere, None);
        assert_eq!(relative, None);
    }

    #[test]
    fn the_proposed_folder_takes_a_number_as_the_daemon_does_when_another_account_has_it() {
        let mut state = state_with(Vec::new());
        three_accounts(&mut state);
        let unix = proposed_folder(&state, MachineId::THIS, "Work");
        state.machines.this.accounts = vec![
            named(AccountId::DEFAULT, "Default", r"C:\Users\sam\.claude"),
            named(work(), "Work", r"C:\Users\sam\.Claude-Work"),
            named(lab(), "Work 2", r"c:/users/sam/.claude-work-2"),
        ];
        let windows = proposed_folder(&state, MachineId::THIS, "work");

        assert_eq!(unix.as_deref(), Some("/home/sam/.claude-work-2"));
        assert_eq!(windows.as_deref(), Some(r"C:\Users\sam\.claude-work-3"));
    }

    /// The new agent dialog with a typed folder and message, a picked account, model and mode,
    /// and its account menu open, as "Manage accounts" finds it.
    fn new_agent_dialog(state: &mut AppState, drafts: &mut Drafts) -> NewAgentForm {
        three_accounts(state);
        actions::open_new_agent(state, drafts);
        drafts.new_agent.folder = "/work/app".to_owned();
        drafts.new_agent.prompt = "Fix the parser".to_owned();
        pick_new_agent_account(state, drafts, lab());
        actions::with_new_agent_form(state, |form| {
            form.model = Some("opus".to_owned());
            form.mode = Some(PermissionMode::AcceptEdits);
            form.menu = Some(FormMenu::Account);
        });
        let Some(Dialog::NewAgent(form)) = state.ui.dialog.clone() else {
            panic!("expected the new agent dialog");
        };
        NewAgentForm { menu: None, ..form }
    }

    #[test]
    fn accounts_opened_from_the_new_agent_dialog_go_back_to_it_as_it_was() {
        let mut state = state_with(Vec::new());
        let mut drafts = Drafts::default();
        let form = new_agent_dialog(&mut state, &mut drafts);
        let typed = drafts.new_agent.clone();
        let back = Some(Dialog::NewAgent(form.clone()));

        open_accounts(&mut state, None);
        let held = state.ui.dialog.clone();
        state.ui.close_dialog();
        let after_close = state.ui.dialog.clone();
        open_accounts(&mut state, None);
        let escaped = state.ui.close_overlays();
        let after_escape = state.ui.dialog.clone();
        open_accounts(&mut state, None);
        open_add_account(&mut state, &mut drafts);
        close_add_account(&mut state);
        let after_done = state.ui.dialog.clone();
        state.ui.close_dialog();
        let after_done_and_close = state.ui.dialog.clone();
        open_accounts(&mut state, None);
        open_add_account(&mut state, &mut drafts);
        state.ui.close_overlays();
        let after_add_escape = state.ui.dialog.clone();

        assert_eq!(
            held,
            Some(Dialog::Accounts(AccountsView {
                machine: MachineId::THIS,
                expanded: None,
                from_new_agent: Some(form.clone()),
            }))
        );
        assert_eq!(after_close, back);
        assert!(escaped);
        assert_eq!(after_escape, back);
        assert_eq!(
            after_done,
            Some(Dialog::Accounts(AccountsView {
                machine: MachineId::THIS,
                expanded: None,
                from_new_agent: Some(form),
            }))
        );
        assert_eq!(after_done_and_close, back);
        assert_eq!(after_add_escape, back);
        assert_eq!(drafts.new_agent, typed);
    }

    #[test]
    fn accounts_opened_elsewhere_close_to_nothing() {
        let mut state = state_with(Vec::new());
        three_accounts(&mut state);
        let mut drafts = Drafts::default();

        open_accounts(&mut state, Some(work()));
        state.ui.close_dialog();
        let after_close = state.ui.dialog.clone();
        open_accounts(&mut state, None);
        let escaped = state.ui.close_overlays();
        let after_escape = state.ui.dialog.clone();
        open_accounts(&mut state, None);
        open_add_account(&mut state, &mut drafts);
        close_add_account(&mut state);
        state.ui.close_dialog();
        let after_add = state.ui.dialog.clone();

        assert_eq!(after_close, None);
        assert!(escaped);
        assert_eq!(after_escape, None);
        assert_eq!(after_add, None);
    }

    #[test]
    fn the_new_agent_dialog_behind_accounts_follows_its_start() {
        let mut state = state_with(Vec::new());
        let mut drafts = Drafts::default();
        let form = new_agent_dialog(&mut state, &mut drafts);
        actions::with_new_agent_form(&mut state, |form| form.starting = true);

        open_accounts(&mut state, None);
        if let Some(form) = state.ui.new_agent_form_mut() {
            form.starting = false;
        }
        state.ui.close_dialog();
        let after_refusal = state.ui.dialog.clone();
        open_accounts(&mut state, None);
        state.ui.new_agent_started();
        state.ui.close_dialog();

        assert_eq!(after_refusal, Some(Dialog::NewAgent(form)));
        assert_eq!(state.ui.dialog, None);
    }

    #[test]
    fn shortcuts_keep_the_new_agent_dialog_behind_accounts() {
        let mut state = state_with(vec![in_folder(1, "/work/app", work())]);
        state.ui.selected = Some(AgentId::new(1));
        let mut drafts = Drafts::default();
        let form = new_agent_dialog(&mut state, &mut drafts);
        let typed = drafts.new_agent.clone();
        let held = Some(Dialog::Accounts(AccountsView {
            machine: MachineId::THIS,
            expanded: None,
            from_new_agent: Some(form.clone()),
        }));

        open_accounts(&mut state, None);
        actions::toggle_switcher(&mut state, &mut drafts);
        let after_switcher = state.ui.dialog.clone();
        crate::state::agent_map::toggle(&mut state);
        let after_map = state.ui.dialog.clone();
        actions::open_new_agent(&mut state, &mut drafts);

        assert_eq!(after_switcher, held);
        assert_eq!(after_map, held);
        assert_eq!(state.ui.dialog, Some(Dialog::NewAgent(form)));
        assert_eq!(drafts.new_agent, typed);
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

        let same_name = rename_account(&state, MachineId::THIS, work(), " Work ");
        let empty = rename_account(&state, MachineId::THIS, work(), "  ");
        let renamed = rename_account(&state, MachineId::THIS, work(), "Job");
        let key = set_environment_credentials(MachineId::THIS, work(), true);
        set_color(&mut state, MachineId::THIS, lab(), AccountColor::Green);

        assert_eq!(
            opened,
            Some(Dialog::Accounts(AccountsView {
                machine: MachineId::THIS,
                expanded: Some(work()),
                from_new_agent: None,
            }))
        );
        assert_eq!(closed, Some(Dialog::Accounts(AccountsView::default())));
        assert_eq!(same_name, []);
        assert_eq!(empty, []);
        assert!(matches!(
            renamed.as_slice(),
            [Outgoing { request: Request::AccountsChange(AccountChangeParams { name: Some(name), credentials: None, .. }), .. }] if name == "Job"
        ));
        assert!(matches!(
            key.as_slice(),
            [Outgoing {
                request: Request::AccountsChange(AccountChangeParams {
                    name: None,
                    credentials: Some(CredentialSource::Environment),
                    ..
                }),
                ..
            }]
        ));
        assert_eq!(color(&state, MachineId::THIS, lab()), AccountColor::Green);
        assert_eq!(
            remove_block(&state, MachineId::THIS, AccountId::DEFAULT),
            Some(RemoveBlock::Default)
        );
        assert_eq!(
            remove_block(&state, MachineId::THIS, work()),
            Some(RemoveBlock::InUse(1))
        );
        assert_eq!(remove_block(&state, MachineId::THIS, lab()), None);
        assert_eq!(remove_account(&mut state, MachineId::THIS, work()), []);
        assert!(matches!(
            remove_account(&mut state, MachineId::THIS, lab()).as_slice(),
            [Outgoing { request: Request::AccountsRemove(AccountParams { account }), .. }] if *account == lab()
        ));
    }

    #[test]
    fn the_sign_in_states_are_asked_once_then_again_on_check() {
        let mut state = state_with(Vec::new());
        let without_accounts = list_request(&mut state, MachineId::THIS);
        three_accounts(&mut state);
        let first = list_request(&mut state, MachineId::THIS);
        let while_asked = list_request(&mut state, MachineId::THIS);
        listed(
            &mut state,
            MachineId::THIS,
            Ok(ResponseBody::Accounts {
                accounts: Vec::new(),
                environment_credentials: EnvironmentCredentials::Set,
            }),
        );
        let after = list_request(&mut state, MachineId::THIS);
        check_again(&mut state, MachineId::THIS);
        let again = list_request(&mut state, MachineId::THIS);

        assert_eq!(without_accounts, None);
        assert!(matches!(
            first,
            Some(Outgoing {
                request: Request::AccountsList,
                ..
            })
        ));
        assert_eq!(while_asked, None);
        assert_eq!(
            state.machines.this.sign_ins.environment_credentials,
            EnvironmentCredentials::Set
        );
        assert_eq!(after, None);
        assert!(again.is_some());
    }

    #[test]
    fn an_account_change_while_the_daemon_is_asked_asks_it_again() {
        let mut state = state_with(Vec::new());
        three_accounts(&mut state);
        let asked = list_request(&mut state, MachineId::THIS);
        let accounts = state.machines.this.accounts.clone();
        super::super::apply::apply(
            &mut state,
            &mut Drafts::default(),
            EventRecord {
                seq: Seq::new(1),
                event: Event::AccountsChanged { accounts },
            },
            MachineId::THIS,
        );
        listed(
            &mut state,
            MachineId::THIS,
            Ok(ResponseBody::Accounts {
                accounts: Vec::new(),
                environment_credentials: EnvironmentCredentials::Unset,
            }),
        );
        let again = list_request(&mut state, MachineId::THIS);
        listed(
            &mut state,
            MachineId::THIS,
            Ok(ResponseBody::Accounts {
                accounts: Vec::new(),
                environment_credentials: EnvironmentCredentials::Unset,
            }),
        );

        assert!(asked.is_some());
        assert!(again.is_some());
        assert_eq!(list_request(&mut state, MachineId::THIS), None);
    }

    fn transcript_item(id: u32, body: ItemBody) -> TranscriptItem {
        TranscriptItem {
            id: ItemId::new(id),
            parent: None,
            body,
        }
    }

    fn failed_turn(outcome: TurnOutcome) -> ItemBody {
        ItemBody::TurnEnd(TurnEnd {
            duration: Duration::from_millis(900),
            cost: CostUsd::new(0.0),
            usage: TokenUsage::default(),
            outcome,
            error: Some("The account is not signed in".to_owned()),
        })
    }

    fn message(text: &str) -> ItemBody {
        ItemBody::UserMessage(UserMessage {
            text: text.to_owned(),
            images: Vec::new(),
            sent_at: None,
        })
    }

    #[test]
    fn a_turn_without_login_shows_its_card_until_a_message_goes_after_it() {
        let mut waiting = in_folder(1, "/work/app", work());
        waiting.sign_in_wait = SignInWait::Waiting;
        waiting.status = AgentStatus::NeedsYou;
        let mut state = state_with(vec![waiting.clone()]);
        three_accounts(&mut state);
        let notice = ItemBody::Notice {
            text: "The account Work is not signed in.".to_owned(),
            detail: None,
            kind: NoticeKind::Plain,
        };
        let entry = state.agents.get_mut(&AgentId::new(1)).unwrap();
        entry.transcript.merge_page(HistoryPage {
            items: vec![
                transcript_item(1, message("Export the invoices")),
                transcript_item(2, notice),
                transcript_item(3, failed_turn(TurnOutcome::NotSignedIn)),
            ],
            older_items: OlderItems::None,
        });

        let card = sign_in_turn(state.agents.get(&AgentId::new(1)).unwrap());
        let line = sign_in_line(&state, state.agents.get(&AgentId::new(1)).unwrap());
        let while_waiting = send_again(&mut state, AgentId::new(1));
        let entry = state.agents.get_mut(&AgentId::new(1)).unwrap();
        entry.summary.sign_in_wait = SignInWait::NotWaiting;
        entry.summary.status = AgentStatus::Idle;
        let signed_in_line = sign_in_line(&state, state.agents.get(&AgentId::new(1)).unwrap());
        let sent = send_again(&mut state, AgentId::new(1));
        let sent_line = sign_in_line(&state, state.agents.get(&AgentId::new(1)).unwrap());
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
                    sent_at: None,
                }),
            })
        );
        assert_eq!(
            line,
            Some(SignInLine::NotSignedIn("Work is not signed in".to_owned()))
        );
        assert_eq!(signed_in_line, Some(SignInLine::SendAgain));
        assert_eq!(
            signed_in_line.as_ref().map(SignInLine::text),
            Some("Signed in: send the message again")
        );
        assert_eq!(sent_line, None, "the line goes once the message is sent");
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
        entry.transcript.merge_page(HistoryPage {
            items: vec![
                transcript_item(1, message("Export the invoices")),
                transcript_item(2, failed_turn(TurnOutcome::NotSignedIn)),
            ],
            older_items: OlderItems::None,
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
            Method::AgentPrompt,
            purpose.clone(),
            Err(RequestError::Disconnected),
            MachineId::THIS,
        );
        let after_failure = send_again(&mut state, agent);

        assert_eq!(second, []);
        assert_eq!(after_failure, first);
    }

    #[test]
    fn the_message_goes_again_while_waiting_when_the_daemon_cannot_tell_the_sign_in() {
        let mut waiting = in_folder(1, "/work/app", work());
        waiting.sign_in_wait = SignInWait::Waiting;
        waiting.status = AgentStatus::NeedsYou;
        let mut state = state_with(vec![waiting]);
        three_accounts(&mut state);
        let agent = AgentId::new(1);
        let entry = state.agents.get_mut(&agent).unwrap();
        entry.transcript.merge_page(HistoryPage {
            items: vec![
                transcript_item(1, message("Export the invoices")),
                transcript_item(2, failed_turn(TurnOutcome::NotSignedIn)),
            ],
            older_items: OlderItems::None,
        });

        signed(&mut state, work(), SignIn::NotSignedIn);
        let not_signed_in = send_again(&mut state, agent);
        signed(&mut state, work(), SignIn::Unknown);
        let first = send_again(&mut state, agent);
        let second = send_again(&mut state, agent);

        assert_eq!(not_signed_in, []);
        assert!(matches!(
            first.as_slice(),
            [Outgoing { request: Request::AgentPrompt(PromptParams { prompt, .. }), .. }] if prompt.text == "Export the invoices"
        ));
        assert_eq!(second, [], "the card sends the message once");
    }

    /// A check that finds the login, written before the failed turn, does not end the wait: the
    /// card still lets the message go again, since a refusal can pass.
    #[test]
    fn the_message_goes_again_while_waiting_when_a_check_finds_the_login() {
        let mut waiting = in_folder(1, "/work/app", work());
        waiting.sign_in_wait = SignInWait::Waiting;
        waiting.status = AgentStatus::NeedsYou;
        let mut state = state_with(vec![waiting]);
        three_accounts(&mut state);
        let agent = AgentId::new(1);
        let entry = state.agents.get_mut(&agent).unwrap();
        entry.transcript.merge_page(HistoryPage {
            items: vec![
                transcript_item(1, message("Export the invoices")),
                transcript_item(2, failed_turn(TurnOutcome::NotSignedIn)),
            ],
            older_items: OlderItems::None,
        });

        signed(&mut state, work(), SignIn::NotSignedIn);
        let not_found = sends_again(&state, agent);
        signed(&mut state, work(), SignIn::SignedIn);
        let found = sends_again(&state, agent);

        assert!(!not_found);
        assert!(found);
    }

    #[test]
    fn the_message_goes_again_while_waiting_when_the_credentials_come_from_the_environment() {
        let mut waiting = in_folder(1, "/work/app", lab());
        waiting.sign_in_wait = SignInWait::Waiting;
        waiting.status = AgentStatus::NeedsYou;
        let mut state = state_with(vec![waiting]);
        three_accounts(&mut state);
        signed(&mut state, lab(), SignIn::NotSignedIn);
        let agent = AgentId::new(1);
        let entry = state.agents.get_mut(&agent).unwrap();
        entry.transcript.merge_page(HistoryPage {
            items: vec![
                transcript_item(1, message("Export the invoices")),
                transcript_item(2, failed_turn(TurnOutcome::NotSignedIn)),
            ],
            older_items: OlderItems::None,
        });

        let shown = sends_again(&state, agent);
        let first = send_again(&mut state, agent);
        let second = send_again(&mut state, agent);

        assert!(shown);
        assert!(matches!(
            first.as_slice(),
            [Outgoing { request: Request::AgentPrompt(PromptParams { prompt, .. }), .. }] if prompt.text == "Export the invoices"
        ));
        assert_eq!(second, [], "the card sends the message once");
    }

    #[test]
    fn only_an_environment_account_with_a_key_or_token_set_had_them_refused() {
        let mut state = state_with(Vec::new());
        three_accounts(&mut state);

        state.machines.this.sign_ins.environment_credentials = EnvironmentCredentials::Unset;
        let without = credentials_refused(&state, MachineId::THIS, lab());
        state.machines.this.sign_ins.environment_credentials = EnvironmentCredentials::Set;
        let with = credentials_refused(&state, MachineId::THIS, lab());
        let signed_in_account = credentials_refused(&state, MachineId::THIS, work());

        assert!(
            !without,
            "with nothing set, Claude Code used the folder's login"
        );
        assert!(with);
        assert!(!signed_in_account);
    }

    #[test]
    fn a_later_turn_with_a_login_shows_no_card() {
        let mut state = state_with(vec![in_folder(1, "/work/app", work())]);
        let entry = state.agents.get_mut(&AgentId::new(1)).unwrap();
        entry.transcript.merge_page(HistoryPage {
            items: vec![
                transcript_item(1, message("Export")),
                transcript_item(2, failed_turn(TurnOutcome::NotSignedIn)),
                transcript_item(3, message("Export")),
                transcript_item(4, failed_turn(TurnOutcome::Failed)),
            ],
            older_items: OlderItems::None,
        });

        assert_eq!(
            sign_in_turn(state.agents.get(&AgentId::new(1)).unwrap()),
            None
        );
        assert_eq!(
            sign_in_line(&state, state.agents.get(&AgentId::new(1)).unwrap()),
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
        waiting.sign_in_wait = SignInWait::Waiting;

        let alert = super::super::apply::apply(
            &mut state,
            &mut drafts,
            EventRecord {
                seq: Seq::new(1),
                event: Event::AgentUpdated(waiting),
            },
            MachineId::THIS,
        );

        assert_eq!(
            alert.map(|alert| alert.body).as_deref(),
            Some("The account Work is not signed in.")
        );
    }
}
