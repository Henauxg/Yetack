//! The screens that update a machine's daemon: in the "No daemon answers" screen's place for this
//! computer alone, as a dialog otherwise, and the confirmation that names busy agents.
//!
//! Each screen is a card: the machine's icon with a title and a lead, then what the screen is
//! about, then its buttons, which "Details" starts when the screen has details to fold open.

use std::cmp::Ordering;

use cockpit_protocol::{AgentId, AgentStatus, PROTOCOL_VERSION};
use dioxus::prelude::*;

use crate::connection::Cockpit;
use crate::daemon_label::DaemonPlace;
use crate::format;
use crate::machine::MachineId;
use crate::state::daemon_update::{self, DaemonUpdate, FailedUpdate, UpdateProgress};
use crate::state::machines::MachineName;
use crate::state::no_daemon::DaemonChoice;
use crate::state::update_offer::{self, GapKind, ProgramBuild, UpdateOffer};
use crate::state::update_plan::{UpdatePlan, UpdateStep};
use crate::state::{AppState, ConnectionState, accounts};
use crate::ui::icons::{Icon, IconKind};
use crate::ui::machines::machine_icon;
use crate::ui::no_daemon::{EveryTimeTick, IN_APP_DETAIL};
use crate::ui::presence::use_presence;

const TITLE_ID: &str = "no-daemon-title";

/// What happens to the agents on a restart, as the cards and the confirmation say it.
const AGENTS_RESUME: &str =
    "A working agent stops its current turn, and every agent resumes its conversation after.";

/// The screen `machine`'s state calls for: a card that offers the update, the update waiting or
/// under way, or how it failed.
#[component]
pub fn DaemonUpdateScreens(machine: MachineId) -> Element {
    let cockpit: Cockpit = use_context();
    let state = cockpit.state.read();
    let Some(known) = state.machines.get(machine) else {
        return rsx! {};
    };
    let MachineName { name, local } = MachineName::new(machine, known);
    let offer = update_offer::offer(&state, machine);
    let body = match (known.update.clone(), offer) {
        (Some(DaemonUpdate::Running(progress) | DaemonUpdate::Reconnecting(progress)), _) => {
            rsx! {
                ProgressCard { machine, name, local, progress }
            }
        }
        (Some(DaemonUpdate::Waiting), _) => rsx! {
            WaitingCard { machine, name, local }
        },
        (Some(DaemonUpdate::Failed(failed)), _) => rsx! {
            FailedCard { machine, name, local, failed }
        },
        (None, Some(offer)) => match offer.kind {
            GapKind::OlderProtocol => rsx! {
                OlderCard { machine, name, local, offer }
            },
            GapKind::NewerProtocol => rsx! {
                NewerCard { machine, name, local, offer }
            },
            GapKind::OtherBuild => rsx! {
                AvailableCard { machine, name, local, offer }
            },
            GapKind::NewerRelease => rsx! {
                NewerReleaseCard { name, local, offer }
            },
        },
        (None, None) => return rsx! {},
    };
    rsx! {
        div {
            class: "no-daemon-card update-card",
            role: "dialog",
            aria_labelledby: TITLE_ID,
            onclick: move |event| event.stop_propagation(),
            {body}
        }
    }
}

/// What the machine's choice on the "No daemon answers" screen allows. Only this computer has one.
fn screen_choice(cockpit: Cockpit, machine: MachineId) -> (Option<DaemonChoice>, bool) {
    if machine != MachineId::THIS {
        return (None, false);
    }
    match &cockpit.state.read().machines.this.connection {
        ConnectionState::NoDaemon(screen) => (screen.busy, screen.every_time),
        ConnectionState::Connecting { .. }
        | ConnectionState::Connected
        | ConnectionState::Disconnected { .. } => (None, false),
    }
}

/// What "Update" does to `name`, as the card's note says it.
fn what_the_update_does(plan: &UpdatePlan, name: &str, app: &ProgramBuild) -> String {
    match plan {
        UpdatePlan::Server(_) => format!(
            "The update builds {} on {name} from its checkout, then restarts its daemon. {AGENTS_RESUME}",
            app.version
        ),
        UpdatePlan::Service(_) => format!(
            "The update restarts {name}'s background service with this app's program. {AGENTS_RESUME}"
        ),
    }
}

/// A card's head: the machine's icon, then the title over what it means.
#[component]
fn CardHead(local: bool, title: String, lead: String) -> Element {
    rsx! {
        div { class: "update-card-head",
            span { class: "update-card-icon", aria_hidden: true,
                Icon { kind: machine_icon(local), size: 20 }
            }
            div { class: "update-card-titles",
                h1 { id: TITLE_ID, "{title}" }
                p { class: "update-card-lead", "{lead}" }
            }
        }
    }
}

/// The daemon's build beside this app's. An arrow between them shows the way an update goes,
/// when the daemon is the older one.
#[component]
fn VersionPair(name: String, offer: UpdateOffer) -> Element {
    let daemon_class = match offer.kind {
        GapKind::OlderProtocol => "version-box behind-protocol",
        GapKind::OtherBuild => "version-box behind",
        GapKind::NewerRelease | GapKind::NewerProtocol => "version-box",
    };
    rsx! {
        div { class: "version-pair",
            VersionBox {
                class: daemon_class,
                who: name,
                program: offer.daemon.clone(),
                protocol: offer.daemon_protocol,
            }
            span { class: "version-arrow", aria_hidden: true,
                if !offer.kind.is_newer() {
                    Icon { kind: IconKind::ArrowRight, size: 18 }
                }
            }
            VersionBox {
                class: "version-box",
                who: "This app".to_owned(),
                program: Some(offer.app.clone()),
                protocol: PROTOCOL_VERSION,
            }
        }
    }
}

/// One side of the pair: who runs it, its release, its commit and its protocol. A daemon of
/// protocol 8 or older does not name its build as it refuses the app.
#[component]
fn VersionBox(
    class: &'static str,
    who: String,
    program: Option<ProgramBuild>,
    protocol: u16,
) -> Element {
    let release = program.as_ref().map_or_else(
        || "Unknown version".to_owned(),
        |build| build.version.to_string(),
    );
    let meta = match program.as_ref().and_then(|build| build.commit.as_ref()) {
        Some(commit) => format!("{} · protocol {protocol}", commit.short()),
        None => format!("protocol {protocol}"),
    };
    rsx! {
        div { class,
            span { class: "version-who", "{who}" }
            span { class: "version-number", "{release}" }
            span { class: "version-meta mono", "{meta}" }
        }
    }
}

/// A note that explains what the card's main button does.
#[component]
fn Callout(text: String) -> Element {
    rsx! {
        div { class: "update-callout",
            span { class: "callout-icon", aria_hidden: true,
                Icon { kind: IconKind::Info, size: 16 }
            }
            span { "{text}" }
        }
    }
}

/// A card's buttons, at its end. "Details" starts the row when the card has details: it folds
/// them open under the row, and the card grows with them.
#[component]
fn CardActions(details: Option<String>, children: Element) -> Element {
    let mut open = use_signal(|| false);
    let unfolded = details.clone().filter(|_| open());
    let shown = use_presence(unfolded);
    rsx! {
        div { class: "update-card-actions",
            if details.is_some() {
                button {
                    class: if open() { "details-link open" } else { "details-link" },
                    aria_expanded: open(),
                    onclick: move |_| open.toggle(),
                    Icon { kind: IconKind::ChevronRight, size: 12 }
                    "Details"
                }
            }
            {children}
        }
        if let Some(shown) = shown {
            div {
                class: if shown.leaving { "update-details-fold leaving" } else { "update-details-fold" },
                inert: shown.leaving.then_some(true),
                div { class: "update-details-clip",
                    pre { class: "update-details", "{shown.value}" }
                }
            }
        }
    }
}

#[component]
fn OlderCard(machine: MachineId, name: String, local: bool, offer: UpdateOffer) -> Element {
    let cockpit: Cockpit = use_context();
    let (busy, every_time) = screen_choice(cockpit, machine);
    let waiting = busy.is_some();
    let on_this_computer = machine == MachineId::THIS;
    let over_ssh = cockpit
        .state
        .read()
        .machines
        .get(machine)
        .is_some_and(|known| known.daemon.place == DaemonPlace::Ssh);
    let lead =
        format!("The app can reach {name}'s agents again once {name} runs this app's version.");
    rsx! {
        CardHead { local, title: format!("{name} runs an older version"), lead }
        VersionPair { name: name.clone(), offer: offer.clone() }
        match &offer.plan {
            Some(plan) => rsx! {
                Callout { text: what_the_update_does(plan, &name, &offer.app) }
            },
            None => rsx! {
                p { class: "no-daemon-hint",
                    if over_ssh {
                        "This app does not know which version it is, so it cannot update {name}."
                    } else {
                        "This app cannot tell which state folder this daemon serves, so it cannot restart it."
                    }
                }
            },
        }
        CardActions { details: Some(offer.details.clone()),
            if on_this_computer {
                InAppButton { busy }
            }
            RetryButton { machine, primary: offer.plan.is_none(), busy }
            if offer.plan.is_some() {
                button {
                    class: "button primary",
                    disabled: waiting,
                    onclick: move |_| cockpit.update_daemon(machine),
                    "Update {name}"
                }
            }
        }
        if on_this_computer {
            EveryTimeTick { every_time, waiting }
        }
    }
}

#[component]
fn NewerCard(machine: MachineId, name: String, local: bool, offer: UpdateOffer) -> Element {
    let cockpit: Cockpit = use_context();
    let (busy, _) = screen_choice(cockpit, machine);
    rsx! {
        CardHead {
            local,
            title: format!("{name} runs a newer version"),
            lead: format!("This app is older than the daemon on {name}, so they cannot connect. Update this app to connect."),
        }
        VersionPair { name: name.clone(), offer: offer.clone() }
        CardActions { details: Some(offer.details.clone()),
            if machine == MachineId::THIS {
                InAppButton { busy }
            }
            RetryButton { machine, primary: true, busy }
        }
    }
}

/// A machine the app uses, whose daemon runs another build than the app's.
#[component]
fn AvailableCard(machine: MachineId, name: String, local: bool, offer: UpdateOffer) -> Element {
    let cockpit: Cockpit = use_context();
    let Some(plan) = offer.plan.clone() else {
        return rsx! {};
    };
    rsx! {
        CardHead {
            local,
            title: format!("Update available for {name}"),
            lead: format!("{name}'s agents keep working until you update it."),
        }
        VersionPair { name: name.clone(), offer: offer.clone() }
        Callout { text: what_the_update_does(&plan, &name, &offer.app) }
        CardActions { details: Some(offer.details.clone()),
            button {
                class: "button",
                onclick: move |_| cockpit.update(|state| state.ui.close_dialog()),
                "Not now"
            }
            button {
                class: "button primary",
                onclick: move |_| cockpit.update_daemon(machine),
                "Update {name}"
            }
        }
    }
}

/// A machine the app uses, whose daemon is of a newer release than the app.
#[component]
fn NewerReleaseCard(name: String, local: bool, offer: UpdateOffer) -> Element {
    let cockpit: Cockpit = use_context();
    rsx! {
        CardHead {
            local,
            title: format!("{name} runs a newer version"),
            lead: "Update this app to match it.".to_owned(),
        }
        VersionPair { name: name.clone(), offer: offer.clone() }
        CardActions { details: Some(offer.details.clone()),
            button {
                class: "button",
                onclick: move |_| cockpit.update(|state| state.ui.close_dialog()),
                "Close"
            }
        }
    }
}

/// "Retry", as the older-version and the newer-version cards offer it.
#[component]
fn RetryButton(machine: MachineId, primary: bool, busy: Option<DaemonChoice>) -> Element {
    let cockpit: Cockpit = use_context();
    let retrying = busy == Some(DaemonChoice::Retry);
    rsx! {
        button {
            class: if primary { "button primary" } else { "button" },
            disabled: busy.is_some(),
            onclick: move |_| cockpit.retry_machine(machine),
            if retrying { "Retrying…" } else { "Retry" }
        }
    }
}

/// "Run agents in this app", which only this computer's cards offer.
#[component]
fn InAppButton(busy: Option<DaemonChoice>) -> Element {
    let cockpit: Cockpit = use_context();
    let starting = busy == Some(DaemonChoice::InApp);
    rsx! {
        button {
            class: "button",
            disabled: busy.is_some(),
            title: IN_APP_DETAIL,
            onclick: move |_| cockpit.choose_daemon(DaemonChoice::InApp),
            if starting { "Starting…" } else { "Run in this app" }
        }
    }
}

/// A busy agent, as the confirmation and the wait list it.
#[derive(Debug, Clone, PartialEq)]
struct BusyAgent {
    key: String,
    title: String,
    /// The status class of its dot.
    status_class: &'static str,
    /// What it does: "working", "waits for you", "runs 2 background tasks".
    doing: String,
}

/// `agents`, as the lists show them. An idle agent in such a list runs background tasks, or may
/// hold a scheduled job.
fn busy_rows(state: &AppState, agents: Vec<AgentId>) -> Vec<BusyAgent> {
    agents
        .into_iter()
        .filter_map(|agent| state.agents.get(&agent))
        .map(|entry| {
            let background = usize::from(entry.summary.background_running);
            let (status, doing) = match accounts::turn_status(&entry.summary) {
                AgentStatus::NeedsYou => (AgentStatus::NeedsYou, "waits for you".to_owned()),
                AgentStatus::Idle if background > 0 => (
                    AgentStatus::Idle,
                    format!(
                        "runs {}",
                        format::count(background, "background task", "background tasks")
                    ),
                ),
                AgentStatus::Idle => (AgentStatus::Idle, "may hold a scheduled job".to_owned()),
                AgentStatus::Working | AgentStatus::Exited => {
                    (AgentStatus::Working, "working".to_owned())
                }
            };
            BusyAgent {
                key: entry.id().get().to_string(),
                title: entry.summary.title.clone(),
                status_class: format::status_class(status),
                doing,
            }
        })
        .collect()
}

#[component]
fn BusyAgentList(agents: Vec<BusyAgent>) -> Element {
    rsx! {
        ul { class: "busy-agents",
            for agent in agents {
                li { key: "{agent.key}", class: "{agent.status_class}",
                    span { class: "status-dot", aria_hidden: true }
                    span { class: "busy-title", "{agent.title}" }
                    span { class: "busy-doing", "{agent.doing}" }
                }
            }
        }
    }
}

/// An update that starts once the machine's agents are idle.
#[component]
fn WaitingCard(machine: MachineId, name: String, local: bool) -> Element {
    let cockpit: Cockpit = use_context();
    let agents = {
        let state = cockpit.state.read();
        busy_rows(&state, update_offer::running_agents(&state, machine))
    };
    rsx! {
        CardHead {
            local,
            title: format!("{name} updates once its agents are idle"),
            lead: "The update starts by itself once none of these agents works, waits for an answer or runs background tasks.".to_owned(),
        }
        BusyAgentList { agents }
        CardActions { details: None,
            button {
                class: "button",
                onclick: move |_| cockpit.update(|state| daemon_update::cancel_wait(state, machine)),
                "Cancel"
            }
            button {
                class: "button primary",
                onclick: move |_| cockpit.update_daemon_now(machine),
                "Update now"
            }
        }
    }
}

/// Names the machine's busy agents before its update, and what the update does to them. For this
/// computer's service while the app cannot list its agents, it says so instead.
#[component]
pub fn ConfirmUpdateDialog(machine: MachineId) -> Element {
    let cockpit: Cockpit = use_context();
    let (name, confirmation, agents) = {
        let state = cockpit.state.read();
        let Some(known) = state.machines.get(machine) else {
            return rsx! {};
        };
        let name = MachineName::new(machine, known).name;
        let Some(confirmation) = daemon_update::confirmation(&state, machine, &name) else {
            return rsx! {};
        };
        let agents = busy_rows(&state, confirmation.agents.clone());
        (name, confirmation, agents)
    };
    let daemon_update::Confirmation {
        text,
        list_note,
        offers_wait,
        ..
    } = confirmation;
    let close = move |_| cockpit.update(|state| state.ui.close_dialog());
    let focus = move |event: MountedEvent| async move {
        // The dialog may have closed already: nothing to focus then.
        let _ = event.data().set_focus(true).await;
    };
    rsx! {
        div {
            class: "dialog confirm-update",
            role: "alertdialog",
            aria_label: "Update {name}?",
            onclick: move |event| event.stop_propagation(),
            div { class: "dialog-head",
                h2 { "Update {name}?" }
                button { class: "icon-button small", aria_label: "Close", onclick: close,
                    Icon { kind: IconKind::Close, size: 14 }
                }
            }
            p { class: "dialog-text", "{text}" }
            if let Some(note) = list_note {
                p { class: "dialog-text list-note", "{note}" }
            }
            if !agents.is_empty() {
                BusyAgentList { agents }
            }
            div { class: "dialog-actions",
                button { class: "button", onclick: close, "Cancel" }
                if offers_wait {
                    button {
                        class: "button",
                        onclick: move |_| cockpit.update_daemon_now(machine),
                        "Update now"
                    }
                    button {
                        class: "button primary",
                        onmounted: focus,
                        onclick: move |_| cockpit.update_daemon_when_idle(machine),
                        "Update when they are idle"
                    }
                } else {
                    button {
                        class: "button primary",
                        onmounted: focus,
                        onclick: move |_| cockpit.update_daemon_now(machine),
                        "Update now"
                    }
                }
            }
        }
    }
}

#[component]
fn ProgressCard(
    machine: MachineId,
    name: String,
    local: bool,
    progress: UpdateProgress,
) -> Element {
    let cockpit: Cockpit = use_context();
    let app = cockpit.state.read().app_build.clone();
    let UpdateProgress {
        plan,
        step,
        replaces,
        output,
        cancel_asked,
        ..
    } = &progress;
    let output = output
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join("\n");
    let lead = match plan {
        UpdatePlan::Server(_) => {
            "The daemon keeps running until the new version is built. You can close the app: the update continues on the server.".to_owned()
        }
        UpdatePlan::Service(_) => {
            format!("The agents resume once {name} runs version {}.", app.version)
        }
    };
    let labels = StepLabels {
        name: name.clone(),
        short_commit: match plan {
            UpdatePlan::Server(plan) => plan.commit.short().to_owned(),
            UpdatePlan::Service(_) => String::new(),
        },
        replaces: replaces.as_ref().map(ToString::to_string),
        app: app.to_string(),
    };
    rsx! {
        CardHead { local, title: format!("Updating {name}"), lead }
        ol { class: "update-steps",
            for shown in plan.steps().iter().copied() {
                StepRow { key: "{shown:?}", shown, current: *step, label: labels.of(shown) }
            }
        }
        if matches!(plan, UpdatePlan::Server(_)) {
            details { class: "no-daemon-details", open: true,
                summary { "Build output" }
                ScrolledOutput { text: output }
            }
        }
        if progress.offers_cancel() {
            CardActions { details: None,
                button {
                    class: "button",
                    disabled: *cancel_asked,
                    onclick: move |_| cockpit.cancel_daemon_update(machine),
                    if *cancel_asked { "Cancelling when the build ends…" } else { "Cancel the update" }
                }
            }
        }
    }
}

/// What the steps of an update say.
struct StepLabels {
    /// The machine, as the screens name it.
    name: String,
    short_commit: String,
    /// The daemon's build the update replaces, when the app knew it.
    replaces: Option<String>,
    app: String,
}

impl StepLabels {
    fn of(&self, step: UpdateStep) -> String {
        match step {
            UpdateStep::GetVersion => format!("Get version {}", self.short_commit),
            UpdateStep::Build => format!("Build it on {}", self.name),
            UpdateStep::Restart => "Restart the daemon".to_owned(),
            UpdateStep::Stop => match &self.replaces {
                Some(build) => format!("Stop version {build}"),
                None => "Stop the daemon".to_owned(),
            },
            UpdateStep::Start => format!("Start version {}", self.app),
            UpdateStep::Reconnect => "Reconnect".to_owned(),
        }
    }
}

/// A block of the script's output, which stays at its end as it grows.
#[component]
fn ScrolledOutput(text: String) -> Element {
    rsx! {
        div { class: "update-output",
            pre { "{text}" }
        }
    }
}

#[component]
fn StepRow(shown: UpdateStep, current: UpdateStep, label: String) -> Element {
    let (class, mark) = match shown.cmp(&current) {
        Ordering::Less => ("done", rsx! { Icon { kind: IconKind::Check, size: 14 } }),
        Ordering::Equal => (
            "now",
            rsx! { span { class: "spinner", role: "status", aria_label: "In progress" } },
        ),
        Ordering::Greater => ("", rsx! { Icon { kind: IconKind::Circle, size: 12 } }),
    };
    rsx! {
        li { class,
            span { class: "update-mark", {mark} }
            "{label}"
        }
    }
}

#[component]
fn FailedCard(machine: MachineId, name: String, local: bool, failed: FailedUpdate) -> Element {
    let cockpit: Cockpit = use_context();
    let text = daemon_update::failure_text(&failed, &name);
    let stop_hint = failed.stop_hint().map(str::to_owned);
    let listed = {
        let state = cockpit.state.read();
        busy_rows(
            &state,
            daemon_update::failure_agents(&state, machine, &failed),
        )
    };
    let again = match failed.plan {
        UpdatePlan::Server(_) => "Update again",
        UpdatePlan::Service(_) => "Retry",
    };
    rsx! {
        CardHead { local, title: text.title, lead: text.lead }
        if !failed.message.is_empty() {
            ScrolledOutput { text: failed.message.clone() }
        }
        if !listed.is_empty() {
            BusyAgentList { agents: listed }
        }
        if let Some(agent) = stop_hint {
            p { class: "no-daemon-hint",
                "To stop one now, run "
                code { "agents-cockpit stop {agent}" }
                " on the server."
            }
        }
        if let Some(hint) = &text.hint {
            p { class: "no-daemon-hint", "{hint}" }
        }
        CardActions { details: None,
            button {
                class: "button",
                onclick: move |_| cockpit.update(|state| daemon_update::back(state, machine)),
                "Back"
            }
            if failed.offers_update_now() {
                button {
                    class: "button",
                    onclick: move |_| cockpit.update_daemon_now(machine),
                    "Update now"
                }
            }
            button {
                class: "button primary",
                onclick: move |_| cockpit.update_daemon(machine),
                "{again}"
            }
        }
    }
}
