//! The Claude accounts' views: the tag that names an agent's account, the new agent dialog's
//! account picker, the "Accounts" and "Add an account" dialogs, the plan card's accounts, and the
//! card of an agent whose account is not signed in.

use std::time::{Duration, SystemTime};

use cockpit_daemon::{API_KEY_ENV, AUTH_TOKEN_ENV, CLAUDE_CONFIG_DIR_ENV};
use cockpit_protocol::{
    AccountId, AgentId, CredentialSource, EnvironmentCredentials, SignIn, SignInWait, UsageWindow,
    WindowUsage,
};
use dioxus::prelude::*;

use crate::connection::{Clock, Cockpit};
use crate::machine::MachineId;
use crate::path_style::{self, PathStyle};
use crate::prefs::AccountColor;
use crate::state::accounts::{
    self, AccountChoice, AccountLabel, AccountsView, AddAccountForm, Billing, ChoiceDetail,
    RemoveBlock,
};
use crate::state::{AppState, Dialog, FormMenu, actions};
use crate::ui::code::{CopyButton, CopyLook};
use crate::ui::icons::{Icon, IconKind};
use crate::ui::machines::machine_choices;
use crate::ui::presence::Presence;
use crate::{bridge, format};

/// The add dialog's fields, which the page's script fills.
const ADD_NAME_FIELD_ID: &str = "add-account-name";
const ADD_FOLDER_FIELD_ID: &str = "add-account-folder";

fn color_style(color: AccountColor) -> String {
    format!("--acct: {}", color.css())
}

/// The account's name in its color: on a sidebar row's second line, after the header's status,
/// on a past session.
#[component]
pub fn AccountTag(label: AccountLabel) -> Element {
    rsx! {
        span {
            class: "acct-tag",
            style: color_style(label.color),
            title: "Account: {label.name}",
            "{label.name}"
        }
    }
}

#[component]
pub fn AccountDot(color: AccountColor) -> Element {
    rsx! {
        span { class: "acct-dot", style: color_style(color), aria_hidden: true }
    }
}

/// The account's dot and name, as a recent folder shows it.
#[component]
pub fn AccountMini(label: AccountLabel) -> Element {
    rsx! {
        span { class: "acct-mini", title: "Account: {label.name}",
            AccountDot { color: label.color }
            "{label.name}"
        }
    }
}

/// A command to type in a terminal, with a button that copies it.
#[component]
pub fn CommandBox(command: String) -> Element {
    rsx! {
        div { class: "command-box",
            span { class: "command-text", "{command}" }
            CopyButton { text: command, label: "Copy the command", look: CopyLook::Worded }
        }
    }
}

/// "Checked 20 s ago", from the daemon's last answer.
fn checked_text(checked_at: Option<SystemTime>, now: SystemTime) -> String {
    match checked_at.and_then(|checked| now.duration_since(checked).ok()) {
        Some(age) if age < Duration::from_secs(5) => "Checked just now".to_owned(),
        Some(age) => format!("Checked {} ago", format::elapsed(age)),
        None => "Checking".to_owned(),
    }
}

/// Whether the account is signed in, as the daemon last saw it, with "Check again".
#[component]
pub fn SignInStatus(machine: MachineId, account: AccountId) -> Element {
    let cockpit: Cockpit = use_context();
    let Clock(now) = use_context::<Clock>();
    let (sign_in, checked_at, checking) = {
        let state = cockpit.state.read();
        let Some(known) = state.machines.get(machine) else {
            return rsx! {};
        };
        let sign_ins = &known.sign_ins;
        (
            sign_ins.sign_in(account),
            sign_ins.checked_at,
            sign_ins.checking,
        )
    };
    let (class, word, detail) = match sign_in {
        Some(SignIn::SignedIn) => ("signin-status signed ok", "Signed in", String::new()),
        Some(SignIn::NotSignedIn) => (
            "signin-status missing",
            "Not signed in",
            checked_text(checked_at, now()),
        ),
        Some(SignIn::Unknown) => (
            "signin-status",
            "Cannot tell",
            "The login may be in the macOS Keychain".to_owned(),
        ),
        None => ("signin-status", "Checking", String::new()),
    };
    rsx! {
        div { class,
            span { class: "status-dot", aria_hidden: true }
            span { class: "signin-state", "{word}" }
            span { class: "signin-detail", "{detail}" }
            span { class: "spacer" }
            CheckAgain { machine, checking }
        }
    }
}

#[component]
fn CheckAgain(machine: MachineId, checking: bool) -> Element {
    let cockpit: Cockpit = use_context();
    rsx! {
        button {
            class: "button small",
            disabled: checking,
            onclick: move |_| cockpit.update(|state| accounts::check_again(state, machine)),
            if checking {
                span { class: "spinner", aria_hidden: true }
            } else {
                Icon { kind: IconKind::Reconnect, size: 14 }
            }
            "Check again"
        }
    }
}

/// The sidebar foot's button: every account's dot, and the "Accounts" dialog.
#[component]
pub fn AccountsButton() -> Element {
    let cockpit: Cockpit = use_context();
    let dots: Vec<AccountColor> = {
        let state = cockpit.state.read();
        let machine = state.selected_machine();
        if accounts::shows_labels(&state, machine) {
            let labels = accounts::labels(&state, machine).into_iter();
            labels.map(|label| label.color).collect()
        } else {
            Vec::new()
        }
    };
    rsx! {
        button {
            class: "foot-accounts",
            title: "Claude accounts",
            onclick: move |_| cockpit.update(|state| accounts::open_accounts(state, None)),
            if dots.is_empty() {
                Icon { kind: IconKind::Person, size: 13 }
            } else {
                span { class: "dots", aria_hidden: true,
                    for (index, color) in dots.into_iter().enumerate() {
                        span { key: "{index}", class: "acct-dot", style: color_style(color) }
                    }
                }
            }
            "Accounts"
        }
    }
}

/// The new agent dialog's account button, next to its machine, and its menu.
#[component]
pub fn AccountPicker(account: AccountId, open: bool) -> Element {
    let cockpit: Cockpit = use_context();
    let label = {
        let state = cockpit.state.read();
        accounts::label_of(&state, new_agent_machine(&state), account)
    };
    let Some(label) = label else {
        return rsx! {};
    };
    rsx! {
        div { class: "menu-anchor",
            button {
                class: if open { "menu-button acct-button open" } else { "menu-button acct-button" },
                aria_haspopup: "menu",
                aria_expanded: open,
                title: "Account",
                onclick: move |event| {
                    event.stop_propagation();
                    cockpit.update(|state| actions::with_new_agent_form(state, |form| {
                        form.menu = (form.menu != Some(FormMenu::Account)).then_some(FormMenu::Account);
                    }));
                },
                AccountDot { color: label.color }
                "{label.name}"
                Icon { kind: IconKind::ChevronDown, size: 12 }
            }
            Presence { open,
                AccountMenu { account }
            }
        }
    }
}

#[component]
fn AccountMenu(account: AccountId) -> Element {
    let cockpit: Cockpit = use_context();
    let (title, choices) = {
        let state = cockpit.state.read();
        let machine = new_agent_machine(&state);
        (
            accounts::picker_title(&state, machine),
            accounts::choices(&state, machine),
        )
    };
    rsx! {
        div {
            class: "popover menu form-menu account-menu",
            onclick: move |event| event.stop_propagation(),
            span { class: "menu-title", "{title}" }
            for choice in choices {
                AccountItem { key: "{choice.label.id.get()}", chosen: choice.label.id == account, choice }
            }
            div { class: "menu-divider" }
            button {
                class: "menu-item quiet",
                onclick: move |_| cockpit.update(|state| accounts::open_accounts(state, None)),
                Icon { kind: IconKind::Person, size: 14 }
                "Manage accounts"
            }
        }
    }
}

#[component]
fn AccountItem(choice: AccountChoice, chosen: bool) -> Element {
    let cockpit: Cockpit = use_context();
    let id = choice.label.id;
    rsx! {
        button {
            class: if chosen { "acct-item chosen" } else { "acct-item" },
            aria_pressed: chosen,
            onclick: move |_| cockpit.update_with_drafts(|state, drafts| accounts::pick_new_agent_account(state, drafts, id)),
            AccountDot { color: choice.label.color }
            span { class: "acct-item-text",
                span { class: "acct-item-name",
                    "{choice.label.name}"
                    if id.is_default() {
                        span { class: "default-word", "default" }
                    }
                }
                span { class: "acct-item-detail", {choice_detail(choice.detail)} }
            }
            if chosen {
                Icon { kind: IconKind::Check, size: 14 }
            }
        }
    }
}

fn percent_text(percent: Option<u8>) -> String {
    percent.map_or_else(|| "?".to_owned(), |percent| format!("{percent}%"))
}

fn choice_detail(detail: ChoiceDetail) -> Element {
    match detail {
        ChoiceDetail::Environment => {
            rsx! { "API key or token from the environment, no plan limits" }
        }
        ChoiceDetail::NotSignedIn => rsx! {
            span { class: "warn", "Not signed in" }
        },
        ChoiceDetail::Plan {
            five_hour,
            seven_day,
        } => {
            let near = five_hour.is_some_and(|percent| percent >= format::CLOSE_TO_LIMIT_PERCENT);
            let five = percent_text(five_hour);
            let seven = percent_text(seven_day);
            rsx! {
                "Subscription · "
                span { class: if near { "warn" } else { "" }, "5-hour {five}" }
                " · 7-day {seven}"
            }
        }
    }
}

/// The plan card's accounts, the selected agent's first.
#[component]
pub fn PlanAccounts(machine: MachineId, agent_account: AccountId) -> Element {
    let cockpit: Cockpit = use_context();
    let ids = accounts::plan_card_accounts(&cockpit.state.read(), machine, agent_account);
    rsx! {
        div { class: "card-section",
            span { class: "section-title", "Plans" }
            for id in ids {
                PlanAccount { key: "{id.get()}", machine, account: id, current: id == agent_account }
            }
            div { class: "card-links",
                button {
                    class: "link-button small",
                    onclick: move |_| cockpit.update(|state| accounts::open_accounts(state, None)),
                    "Manage accounts"
                }
            }
        }
    }
}

#[component]
fn PlanAccount(machine: MachineId, account: AccountId, current: bool) -> Element {
    let cockpit: Cockpit = use_context();
    let Clock(now) = use_context::<Clock>();
    let (label, billing, windows) = {
        let state = cockpit.state.read();
        let Some(label) = accounts::label_of(&state, machine, account) else {
            return rsx! {};
        };
        let windows: Vec<(UsageWindow, WindowUsage)> =
            accounts::plan_usage(&state, machine, account)
                .map(|usage| {
                    usage
                        .windows
                        .iter()
                        .map(|(window, usage)| (*window, *usage))
                        .collect()
                })
                .unwrap_or_default();
        (label, accounts::billing(&state, machine, account), windows)
    };
    let now = now();
    rsx! {
        div {
            class: if current { "plan-acct current" } else { "plan-acct" },
            div { class: "plan-acct-head",
                AccountDot { color: label.color }
                span { class: "name", "{label.name}" }
                if current {
                    span { class: "this-agent", "This agent" }
                }
                span { class: "kind",
                    match billing {
                        Billing::Environment => rsx! { "API key or token from the environment" },
                        Billing::NotSignedIn => rsx! { span { class: "warning", "Not signed in" } },
                        Billing::Plan => rsx! { "Claude subscription" },
                    }
                }
            }
            match billing {
                Billing::Environment => rsx! {
                    span { class: "muted small", "No plan limits: Claude bills the API key or token per token." }
                },
                Billing::NotSignedIn => rsx! {
                    span { class: "muted small",
                        "Its agents cannot run until you sign in. "
                        button {
                            class: "link-button",
                            onclick: move |_| cockpit.update(|state| accounts::open_accounts(state, Some(account))),
                            "How to sign in"
                        }
                    }
                },
                Billing::Plan if windows.is_empty() => rsx! {
                    span { class: "muted small", "Claude Code has not reported this plan's usage yet." }
                },
                Billing::Plan => rsx! {
                    div { class: "plan-windows",
                        for (window, usage) in windows {
                            PlanWindow { key: "{format::window_label(window)}", window, usage, now }
                        }
                    }
                },
            }
        }
    }
}

#[component]
fn PlanWindow(window: UsageWindow, usage: WindowUsage, now: SystemTime) -> Element {
    let percent = usage.utilization.map(format::percent);
    let width = percent.unwrap_or(0);
    let near = percent.is_some_and(|percent| percent >= format::CLOSE_TO_LIMIT_PERCENT);
    let reset = usage
        .resets_at
        .and_then(|moment| format::reset_time(moment, now))
        .unwrap_or_default();
    rsx! {
        div { class: "plan-window",
            div { class: "card-row",
                span { "{format::window_label(window)}" }
                span { "{percent_text(percent)}" }
            }
            div { class: "bar",
                div { class: if near { "bar-fill warning" } else { "bar-fill accent" }, style: "width: {width}%" }
            }
            span { class: "muted small", title: "{reset}", "{reset}" }
        }
    }
}

/// Six swatches; a click picks one.
#[component]
fn Swatches(chosen: AccountColor, onpick: EventHandler<AccountColor>) -> Element {
    use strum::VariantArray;

    rsx! {
        div { class: "swatches", role: "radiogroup", aria_label: "Color",
            for color in AccountColor::VARIANTS.iter().copied() {
                button {
                    key: "{color.name()}",
                    class: if color == chosen { "swatch chosen" } else { "swatch" },
                    style: color_style(color),
                    role: "radio",
                    aria_checked: color == chosen,
                    title: color.name(),
                    onclick: move |_| onpick.call(color),
                }
            }
        }
    }
}

/// Whether the account takes its credentials from the daemon's environment, and what it means.
#[component]
fn EnvironmentChoice(uses: bool, note: String, onchange: EventHandler<bool>) -> Element {
    rsx! {
        label { class: "check-row",
            input {
                r#type: "checkbox",
                checked: uses,
                onchange: move |_| onchange.call(!uses),
            }
            span { class: "check-text",
                span {
                    "Use the API key or token from my environment ("
                    code { "{API_KEY_ENV}" }
                    ", "
                    code { "{AUTH_TOKEN_ENV}" }
                    ")"
                }
                span { class: "hint", "{note}" }
            }
        }
    }
}

/// Every account, each a card that opens to rename it, change its color or its environment choice,
/// sign in to it again, or remove it.
#[component]
pub fn AccountsDialog(view: AccountsView) -> Element {
    let cockpit: Cockpit = use_context();
    let machine = view.machine;
    let (ids, machines) = {
        let state = cockpit.state.read();
        let ids: Vec<AccountId> = state
            .machines
            .get(machine)
            .map(|known| known.accounts.iter().map(|account| account.id).collect())
            .unwrap_or_default();
        (ids, machine_choices(&state, machine))
    };
    let single = ids.len() < 2;
    let older_daemon = ids.is_empty();
    let close = move |_| cockpit.update(|state| state.ui.close_dialog());
    rsx! {
        div {
            class: "dialog accounts",
            role: "dialog",
            aria_label: "Accounts",
            tabindex: "-1",
            onmounted: move |event| async move {
                // The dialog may have closed already: nothing to focus then.
                let _ = event.data().set_focus(true).await;
            },
            onclick: move |event| event.stop_propagation(),
            div { class: "dialog-head",
                h2 { "Accounts" }
                button { class: "icon-button small", aria_label: "Close", onclick: close,
                    Icon { kind: IconKind::Close, size: 14 }
                }
            }
            div { class: "dialog-body",
                span { class: "dialog-note",
                    "Each account has its own sign-in, Claude Code settings and plan limits. An agent keeps the account it started with."
                    if single {
                        " Add a second account to pick one for each agent."
                    }
                }
                if !machines.is_empty() {
                    div { class: "machine-choice", role: "radiogroup", aria_label: "Machine",
                        for choice in machines {
                            {
                                let picked = choice.machine;
                                rsx! {
                                    button {
                                        key: "{choice.name}",
                                        class: if choice.chosen { "machine-option checked" } else { "machine-option" },
                                        role: "radio",
                                        aria_checked: choice.chosen,
                                        disabled: !choice.answers,
                                        onclick: move |_| cockpit.update(|state| accounts::pick_accounts_machine(state, picked)),
                                        Icon { kind: choice.icon, size: 13 }
                                        "{choice.name}"
                                    }
                                }
                            }
                        }
                    }
                }
                if older_daemon {
                    span { class: "dialog-note", "This daemon is older than accounts: it runs every agent with its own login." }
                }
                div { class: "acct-list",
                    for id in ids {
                        AccountCard { key: "{id.get()}", machine, account: id, open: view.expanded == Some(id) }
                    }
                }
            }
            div { class: "dialog-footer row",
                if !older_daemon {
                    button {
                        class: "button",
                        onclick: move |_| cockpit.update_with_drafts(accounts::open_add_account),
                        Icon { kind: IconKind::Plus, size: 14 }
                        "Add an account"
                    }
                }
                span { class: "spacer" }
                button { class: "button", onclick: close, "Close" }
            }
        }
    }
}

/// What a card needs from the state.
#[derive(Debug, Clone, PartialEq)]
struct CardData {
    label: AccountLabel,
    folder: String,
    uses_environment: bool,
    billing: Billing,
    sign_in: Option<SignIn>,
    command: Option<String>,
    agents: usize,
    blocked: Option<RemoveBlock>,
    machine: String,
}

fn card_data(state: &AppState, machine: MachineId, id: AccountId) -> Option<CardData> {
    let known = state.machines.get(machine)?;
    let account = accounts::account(state, machine, id)?;
    Some(CardData {
        label: accounts::label_of(state, machine, id)?,
        folder: path_style::shown(&account.folder),
        uses_environment: account.credentials == CredentialSource::Environment,
        billing: accounts::billing(state, machine, id),
        sign_in: known.sign_ins.sign_in(id),
        command: known.sign_ins.command(id).map(str::to_owned),
        agents: accounts::agent_count(state, machine, id),
        blocked: accounts::remove_block(state, machine, id),
        machine: known.daemon.machine.clone(),
    })
}

/// The machine the new agent dialog starts its agent on, this computer without the dialog.
fn new_agent_machine(state: &AppState) -> MachineId {
    match &state.ui.dialog {
        Some(Dialog::NewAgent(form)) => form.machine,
        _ => MachineId::THIS,
    }
}

#[component]
fn AccountCard(machine: MachineId, account: AccountId, open: bool) -> Element {
    let cockpit: Cockpit = use_context();
    let Some(data) = card_data(&cockpit.state.read(), machine, account) else {
        return rsx! {};
    };
    let agents = format::count(data.agents, "agent", "agents");
    let (state_class, state_text) = match (data.billing, data.sign_in) {
        (Billing::Environment, _) => ("acct-card-state key", "API key"),
        (_, Some(SignIn::NotSignedIn)) => ("acct-card-state missing", "Not signed in"),
        (_, Some(SignIn::SignedIn)) => ("acct-card-state ok", "Signed in"),
        (_, Some(SignIn::Unknown)) => ("acct-card-state", "Cannot tell"),
        (_, None) => ("acct-card-state", "Checking"),
    };
    let name = data.label.name.clone();
    rsx! {
        div {
            class: if open { "acct-card open" } else { "acct-card" },
            style: color_style(data.label.color),
            button {
                class: "acct-card-head",
                aria_expanded: open,
                onclick: move |_| cockpit.update(|state| accounts::toggle_account(state, account)),
                span { class: "acct-swatch", aria_hidden: true }
                span { class: "acct-card-name",
                    b {
                        "{data.label.name}"
                        if account.is_default() {
                            span { class: "default-tag", "Default" }
                        }
                    }
                    span { class: "acct-card-folder", "{data.folder}" }
                }
                span { class: state_class,
                    span { class: "status-dot", aria_hidden: true }
                    "{state_text}"
                }
                span { class: "acct-card-agents", "{agents}" }
                span { class: if open { "chevron open" } else { "chevron" },
                    Icon { kind: IconKind::ChevronRight, size: 14 }
                }
            }
            if open {
                div { class: "acct-card-body",
                    div { class: "form-row",
                        div { class: "field-group",
                            label { class: "field-label", "Name" }
                            input {
                                // A rename from elsewhere mounts the field afresh, with the new name.
                                key: "{name}",
                                class: "text-input",
                                r#type: "text",
                                spellcheck: false,
                                aria_label: "Account name",
                                initial_value: "{name}",
                                onchange: move |event: FormEvent| {
                                    let typed = event.value();
                                    cockpit.change(|state| accounts::rename_account(state, machine, account, &typed));
                                },
                            }
                        }
                        div { class: "field-group",
                            span { class: "field-label", "Color" }
                            Swatches {
                                chosen: data.label.color,
                                onpick: move |color| cockpit.update(|state| accounts::set_color(state, machine, account, color)),
                            }
                        }
                    }
                    EnvironmentChoice {
                        uses: data.uses_environment,
                        note: if data.uses_environment {
                            "On: its agents bill the API key or token per token. A change applies to the agents it starts from now on.".to_owned()
                        } else {
                            format!("Off: its agents use its sign-in, without {API_KEY_ENV} or {AUTH_TOKEN_ENV}. A change applies to the agents it starts from now on.")
                        },
                        onchange: move |uses| cockpit.change(|_| accounts::set_environment_credentials(machine, account, uses)),
                    }
                    if !data.uses_environment {
                        div { class: "field-group",
                            span { class: "field-label", "Sign in again, or to another login, in a terminal on {data.machine}" }
                            if let Some(command) = data.command.clone() {
                                CommandBox { command }
                            }
                            SignInStatus { machine, account }
                        }
                    }
                    div { class: "acct-card-actions",
                        button {
                            class: "button danger small",
                            disabled: data.blocked.is_some(),
                            onclick: move |_| cockpit.change(|state| accounts::remove_account(state, machine, account)),
                            "Remove"
                        }
                        span { class: "hint",
                            match data.blocked {
                                Some(RemoveBlock::Default) => "The default account is your existing login: it stays.".to_owned(),
                                Some(RemoveBlock::InUse(count)) => format!(
                                    "{} {} {}: close {} first. Removing leaves {} on {}.",
                                    count,
                                    if count == 1 { "agent uses" } else { "agents use" },
                                    data.label.name,
                                    if count == 1 { "it" } else { "them" },
                                    data.folder,
                                    data.machine,
                                ),
                                None => format!("Removing leaves {} on {}.", data.folder, data.machine),
                            }
                        }
                    }
                }
            }
        }
    }
}

/// The two steps of a new account: its name, color, folder and environment choice, then, once the
/// daemon added it, how to sign in to it.
#[component]
pub fn AddAccountDialog(form: AddAccountForm) -> Element {
    let cockpit: Cockpit = use_context();
    let (name, folder) = {
        let drafts = cockpit.drafts.read();
        let draft = &drafts.add_account;
        (draft.name.clone(), draft.folder.clone())
    };
    // The fields start from the drafts, then follow the keyboard alone.
    let name_seed = use_hook(|| name.clone());
    let folder_seed = use_hook(|| folder.clone());
    let (machine, key_is_set, added, windows, proposes) = {
        let state = cockpit.state.read();
        let added = form
            .added
            .and_then(|id| card_data(&state, form.machine, id).map(|data| (id, data)));
        let known = state
            .machines
            .get(form.machine)
            .unwrap_or(&state.machines.this);
        (
            known.daemon.machine.clone(),
            known.sign_ins.environment_credentials == EnvironmentCredentials::Set,
            added,
            known.daemon.path_style() == PathStyle::Windows,
            accounts::proposed_folder(&state, form.machine, "").is_some(),
        )
    };
    let folder_placeholder = if proposes {
        "The account's folder"
    } else {
        "Leave empty for .claude-<name> in the home folder"
    };
    let can_add = !name.trim().is_empty() && !form.adding && form.added.is_none();
    let add =
        move || cockpit.change_with_drafts(|state, drafts| accounts::add_account(state, drafts));
    let add_on_command_enter = move |event: KeyboardEvent| {
        let command = event.modifiers().ctrl() || event.modifiers().meta();
        if command && event.key() == Key::Enter {
            add();
        }
    };
    let close = move |_| cockpit.update(accounts::close_add_account);
    let uses_environment = form.use_environment;
    let key_note = if uses_environment {
        format!(
            "On: this account's agents get {API_KEY_ENV} and {AUTH_TOKEN_ENV} from the daemon's environment, and Claude bills the key or token per token. No sign-in is needed."
        )
    } else {
        format!(
            "Off: this account's agents use its sign-in. The app removes {API_KEY_ENV} and {AUTH_TOKEN_ENV} from their environment, so a subscription is never billed to an API key or token by accident."
        )
    };
    let shell = if windows { "PowerShell" } else { "a terminal" };
    rsx! {
        div {
            class: "dialog add-account",
            role: "dialog",
            aria_label: "Add an account",
            onclick: move |event| event.stop_propagation(),
            div { class: "dialog-head",
                h2 { "Add an account" }
                button { class: "icon-button small", aria_label: "Close", onclick: close,
                    Icon { kind: IconKind::Close, size: 14 }
                }
            }
            div { class: "dialog-body",
                div { class: "step-title", span { class: "step-number", "1" } "The account" }
                if let Some((_, data)) = &added {
                    div { class: "added-account", style: color_style(data.label.color),
                        span { class: "acct-swatch", aria_hidden: true }
                        span { class: "acct-card-name",
                            b { "{data.label.name}" }
                            span { class: "acct-card-folder", "{data.folder}" }
                        }
                        span { class: "acct-card-state ok",
                            Icon { kind: IconKind::Check, size: 14 }
                            "Added"
                        }
                    }
                } else {
                    div { class: "form-row",
                        div { class: "field-group",
                            label { class: "field-label", r#for: ADD_NAME_FIELD_ID, "Name" }
                            input {
                                id: ADD_NAME_FIELD_ID,
                                class: "text-input",
                                r#type: "text",
                                spellcheck: false,
                                placeholder: "Work",
                                initial_value: "{name_seed}",
                                onmounted: move |event| async move {
                                    // The dialog may have closed already: nothing to focus then.
                                    let _ = event.data().set_focus(true).await;
                                },
                                oninput: move |event: FormEvent| {
                                    let folder = cockpit.with_drafts(|state, drafts| accounts::type_account_name(state, drafts, event.value()));
                                    if let Some(folder) = folder {
                                        bridge::set_field_text(ADD_FOLDER_FIELD_ID, folder);
                                    }
                                },
                                onkeydown: add_on_command_enter,
                            }
                        }
                        div { class: "field-group",
                            span { class: "field-label", "Color" }
                            Swatches {
                                chosen: form.color,
                                onpick: move |color| cockpit.update(|state| accounts::with_add_form(state, |form| form.color = color)),
                            }
                        }
                    }
                    div { class: "field-group",
                        label { class: "field-label", r#for: ADD_FOLDER_FIELD_ID, "Config folder on {machine}" }
                        div { class: "folder-field",
                            Icon { kind: IconKind::Folder, size: 16 }
                            input {
                                id: ADD_FOLDER_FIELD_ID,
                                class: "folder-input mono",
                                r#type: "text",
                                spellcheck: false,
                                placeholder: folder_placeholder,
                                initial_value: "{folder_seed}",
                                oninput: move |event: FormEvent| cockpit.with_drafts(|_, drafts| accounts::type_account_folder(drafts, event.value())),
                                onkeydown: add_on_command_enter,
                            }
                        }
                        span { class: "hint",
                            "Claude Code keeps this account's sign-in, settings and sessions here ("
                            span { class: "mono", "{CLAUDE_CONFIG_DIR_ENV}" }
                            "). A folder you already use keeps its sign-in."
                        }
                    }
                    EnvironmentChoice {
                        uses: uses_environment,
                        note: key_note,
                        onchange: move |uses| cockpit.update(|state| accounts::with_add_form(state, |form| form.use_environment = uses)),
                    }
                    if let Some(error) = &form.error {
                        span { class: "form-error", role: "alert", "The daemon did not add the account: {error}" }
                    }
                }
                div { class: "sep" }
                if uses_environment {
                    div { class: "step-title", span { class: "step-number", "2" } "API key or token" }
                    div { class: if key_is_set { "signin-status signed ok" } else { "signin-status missing" },
                        span { class: "status-dot", aria_hidden: true }
                        span { class: "signin-state",
                            if key_is_set {
                                "{API_KEY_ENV} or {AUTH_TOKEN_ENV} is set on {machine}"
                            } else {
                                "Neither {API_KEY_ENV} nor {AUTH_TOKEN_ENV} is set on {machine}"
                            }
                        }
                        span { class: "spacer" }
                        CheckAgain {
                            machine: form.machine,
                            checking: cockpit.state.read().machines.get(form.machine).is_some_and(|known| known.sign_ins.checking),
                        }
                    }
                    span { class: "hint",
                        "The app only checks whether the variables are set in the daemon's environment. It never reads, shows or stores the key or token."
                    }
                } else {
                    div { class: "step-title", span { class: "step-number", "2" } "Sign in" }
                    if let Some((id, data)) = added {
                        span { class: "dialog-note",
                            "Run this in {shell} on "
                            b { "{machine}" }
                            ". Claude Code opens the sign-in page, and exits once you are signed in."
                        }
                        if let Some(command) = data.command {
                            CommandBox { command }
                        }
                        span { class: "hint",
                            "You can sign in with a Claude subscription (Pro, Max, Team or Enterprise), or with an Anthropic Console account, which bills per token like an API key: add "
                            span { class: "mono", "--console" }
                            " to the command for that. The sign-in stays in the account's folder: the app never sees or stores a key."
                        }
                        SignInStatus { machine: form.machine, account: id }
                    } else {
                        span { class: "hint", "The command that signs in shows here once the account is added." }
                    }
                }
            }
            div { class: "dialog-footer row",
                span { class: "hint",
                    if form.added.is_none() && !uses_environment {
                        "You can add it now and sign in later, from Accounts."
                    }
                }
                div { class: "dialog-actions",
                    if form.added.is_some() {
                        button { class: "button primary", onclick: close, "Done" }
                    } else {
                        button { class: "button", onclick: close, "Cancel" }
                        button {
                            class: if can_add { "button primary" } else { "button disabled" },
                            disabled: !can_add,
                            onclick: move |_| add(),
                            if form.adding {
                                span { class: "spinner", aria_hidden: true }
                                "Adding"
                            } else {
                                "Add the account"
                            }
                            span { class: "key-hint", "Ctrl Enter" }
                        }
                    }
                }
            }
        }
    }
}

/// The card in the chat of an agent whose account found no login: how to sign in, whether it is
/// signed in now, and the message to send again.
#[component]
pub fn SignInCard(agent: AgentId) -> Element {
    let cockpit: Cockpit = use_context();
    let card = {
        let state = cockpit.state.read();
        let Some(entry) = state.agents.get(&agent) else {
            return rsx! {};
        };
        let id = entry.summary.account;
        let machine = MachineId::of(agent);
        let data = card_data(&state, machine, id);
        let others = accounts::others_waiting(&state, agent, id);
        let waits = entry.summary.sign_in_wait == SignInWait::Waiting;
        let sends = accounts::sends_again(&state, agent);
        let refused = accounts::credentials_refused(&state, machine, id);
        (id, data, others, waits, sends, refused)
    };
    let (id, data, others, waits, sends, refused) = card;
    let name = data
        .as_ref()
        .map_or_else(|| "its account".to_owned(), |data| data.label.name.clone());
    let folder = data.as_ref().map(|data| data.folder.clone());
    let machine = data.as_ref().map_or_else(
        || "the daemon's machine".to_owned(),
        |data| data.machine.clone(),
    );
    let command = data.and_then(|data| data.command);
    let others_note = match others {
        0 => None,
        1 => Some(format!(
            "One other agent of {name} waits for the same sign-in."
        )),
        count => Some(format!(
            "{count} other agents of {name} wait for the same sign-in."
        )),
    };
    rsx! {
        div { class: if waits { "card waiting signin-card" } else { "card signin-card" },
            div { class: "card-head",
                Icon { kind: IconKind::Warning, size: 16 }
                if refused {
                    span { "Claude Code refused the API key or token of {name}" }
                } else {
                    span { "The account {name} is not signed in" }
                }
            }
            span { class: "card-note",
                "This agent runs with "
                b { "{name}" }
                if let Some(folder) = folder {
                    " ("
                    span { class: "mono", "{folder}" }
                    ")"
                }
                ". Claude Code did not send your message."
            }
            if refused {
                span { class: "card-note",
                    "Check the daemon's {API_KEY_ENV} and {AUTH_TOKEN_ENV}, or turn them off for this account in Accounts and sign in."
                }
            } else {
                span { class: "card-note",
                    "Sign in to {name} in a terminal on "
                    b { "{machine}" }
                    " with this command. It exits once you are signed in."
                }
                if let Some(command) = command {
                    CommandBox { command }
                }
            }
            if waits {
                SignInStatus { machine: MachineId::of(agent), account: id }
            } else {
                div { class: "signin-status signed ok",
                    span { class: "status-dot", aria_hidden: true }
                    span { class: "signin-state", "Signed in" }
                    span { class: "signin-detail", "{name} can run agents again." }
                }
            }
            div { class: "card-actions",
                button {
                    class: if sends { "button primary" } else { "button disabled" },
                    disabled: !sends,
                    onclick: move |_| cockpit.change(|state| accounts::send_again(state, agent)),
                    if sends {
                        span { class: "key-hint", "Enter" }
                    }
                    "Send the message again"
                }
                if let Some(note) = others_note {
                    span { class: "card-note", "{note}" }
                }
            }
        }
    }
}

/// The dialogs this module adds, as `Dialogs` shows them.
pub fn dialog(dialog: Dialog) -> Element {
    match dialog {
        Dialog::Accounts(view) => rsx! {
            AccountsDialog { view }
        },
        Dialog::AddAccount(form) => rsx! {
            AddAccountDialog { form }
        },
        Dialog::Switcher
        | Dialog::NewAgent(_)
        | Dialog::OpenFile(_)
        | Dialog::CloseAgent(_)
        | Dialog::AgentMap(_)
        | Dialog::Machines(_)
        | Dialog::NoDaemon
        | Dialog::DaemonUpdate(_)
        | Dialog::ImageView(_)
        | Dialog::WorkingFolder(_) => rsx! {},
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_check_says_how_long_ago_it_ran() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000);

        assert_eq!(checked_text(Some(now), now), "Checked just now");
        assert_eq!(
            checked_text(Some(now - Duration::from_secs(20)), now),
            "Checked 20 s ago"
        );
        assert_eq!(checked_text(None, now), "Checking");
    }
}
