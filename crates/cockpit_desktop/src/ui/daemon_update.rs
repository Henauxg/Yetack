//! The screens that update a machine's daemon: in the "No daemon answers" screen's place for this
//! computer alone, as a dialog otherwise, and the confirmation that names busy agents.

use std::cmp::Ordering;

use dioxus::prelude::*;

use crate::connection::Cockpit;
use crate::daemon_label::DaemonPlace;
use crate::machine::MachineId;
use crate::state::daemon_update::{
    self, DaemonUpdate, FailedUpdate, UpdateFailure, UpdateProgress,
};
use crate::state::machines::MachineName;
use crate::state::no_daemon::DaemonChoice;
use crate::state::update_offer::{self, GapKind, ProgramBuild, UpdateOffer};
use crate::state::update_plan::{UpdatePlan, UpdateStep};
use crate::state::{AppState, ConnectionState};
use crate::ui::icons::{Icon, IconKind};
use crate::ui::no_daemon::{Details, EveryTimeTick, IN_APP_DETAIL};

const TITLE_ID: &str = "no-daemon-title";

/// How the update screens name `machine`.
fn machine_name(state: &AppState, machine: MachineId) -> Option<String> {
    let known = state.machines.get(machine)?;
    Some(MachineName::new(machine, known).name)
}

/// The screen `machine`'s state calls for: a card that offers the update, the update waiting or
/// under way, or how it failed.
#[component]
pub fn DaemonUpdateScreens(machine: MachineId) -> Element {
    let cockpit: Cockpit = use_context();
    let state = cockpit.state.read();
    let (Some(known), Some(name)) = (state.machines.get(machine), machine_name(&state, machine))
    else {
        return rsx! {};
    };
    let subject = known.daemon.subject();
    let offer = update_offer::offer(&state, machine);
    let body = match (known.update.clone(), offer) {
        (Some(DaemonUpdate::Running(progress) | DaemonUpdate::Reconnecting(progress)), _) => {
            rsx! {
                ProgressCard { machine, name, progress }
            }
        }
        (Some(DaemonUpdate::Waiting), _) => rsx! {
            WaitingCard { machine, name }
        },
        (Some(DaemonUpdate::Failed(failed)), _) => rsx! {
            FailedCard { machine, name, failed }
        },
        (None, Some(offer)) => match offer.kind {
            GapKind::OlderProtocol => rsx! {
                OlderCard { machine, name, subject, offer }
            },
            GapKind::NewerProtocol => rsx! {
                NewerCard { machine, name, subject, offer }
            },
            GapKind::OtherBuild => rsx! {
                AvailableCard { machine, name, offer }
            },
            GapKind::NewerRelease => rsx! {
                NewerReleaseCard { name, offer }
            },
        },
        (None, None) => return rsx! {},
    };
    rsx! {
        div {
            class: "no-daemon-card",
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

/// What "Update" does, under its title.
fn update_detail(plan: &UpdatePlan, app: &ProgramBuild) -> String {
    match plan {
        UpdatePlan::Server(plan) => format!(
            "Builds this app's version ({}) on the server and restarts its daemon. Agents resume after the restart.",
            plan.commit.short()
        ),
        UpdatePlan::Service(_) => format!(
            "Restarts the background service with this app's version ({app}). Agents resume after the restart."
        ),
    }
}

#[component]
fn OlderCard(machine: MachineId, name: String, subject: String, offer: UpdateOffer) -> Element {
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
    rsx! {
        h1 { id: TITLE_ID, "{subject} runs an older version" }
        p { class: "no-daemon-lead",
            "This app and the daemon on {name} are different versions, so they cannot connect."
        }
        if offer.plan.is_none() {
            p { class: "no-daemon-hint",
                if over_ssh {
                    "This app does not know which version it is, so it cannot update {name}."
                } else {
                    "This app cannot tell which state folder this daemon serves, so it cannot restart it."
                }
            }
        }
        Details { error: offer.details.clone(), open: false }
        div { class: "no-daemon-choices",
            if let Some(plan) = &offer.plan {
                OfferButton {
                    primary: true,
                    disabled: waiting,
                    title: format!("Update {name}"),
                    detail: update_detail(plan, &offer.app),
                    onclick: move |()| cockpit.update_daemon(machine),
                }
            }
            RetryOffer { machine, name: name.clone(), primary: offer.plan.is_none(), busy }
            if on_this_computer {
                InAppOffer { busy }
            }
        }
        if on_this_computer {
            EveryTimeTick { every_time, waiting }
        }
    }
}

#[component]
fn NewerCard(machine: MachineId, name: String, subject: String, offer: UpdateOffer) -> Element {
    let cockpit: Cockpit = use_context();
    let (busy, _) = screen_choice(cockpit, machine);
    rsx! {
        h1 { id: TITLE_ID, "{subject} runs a newer version" }
        p { class: "no-daemon-lead",
            "This app is older than the daemon on {name}, so they cannot connect. Update this app to connect."
        }
        Details { error: offer.details.clone(), open: false }
        div { class: "no-daemon-choices",
            RetryOffer { machine, name: name.clone(), primary: true, busy }
            if machine == MachineId::THIS {
                InAppOffer { busy }
            }
        }
    }
}

/// A machine the app uses, whose daemon runs another build than the app's.
#[component]
fn AvailableCard(machine: MachineId, name: String, offer: UpdateOffer) -> Element {
    let cockpit: Cockpit = use_context();
    let Some(plan) = offer.plan.clone() else {
        return rsx! {};
    };
    let runs = offer.daemon.as_ref().map_or_else(
        || "another version".to_owned(),
        |daemon| format!("version {daemon}"),
    );
    rsx! {
        h1 { id: TITLE_ID, "Update available for {name}" }
        p { class: "no-daemon-lead",
            "{name} runs {runs}, and this app version {offer.app}. Its agents keep working until you update it."
        }
        Details { error: offer.details.clone(), open: false }
        div { class: "no-daemon-choices",
            OfferButton {
                primary: true,
                disabled: false,
                title: format!("Update {name}"),
                detail: update_detail(&plan, &offer.app),
                onclick: move |()| cockpit.update_daemon(machine),
            }
            OfferButton {
                primary: false,
                disabled: false,
                title: "Not now".to_owned(),
                detail: "The machine stays in use as it is.".to_owned(),
                onclick: move |()| cockpit.update(|state| state.ui.close_dialog()),
            }
        }
    }
}

/// A machine the app uses, whose daemon is of a newer release than the app.
#[component]
fn NewerReleaseCard(name: String, offer: UpdateOffer) -> Element {
    let cockpit: Cockpit = use_context();
    let runs = offer.daemon.as_ref().map_or_else(
        || "a newer version".to_owned(),
        |daemon| format!("version {daemon}"),
    );
    rsx! {
        h1 { id: TITLE_ID, "{name} runs a newer version" }
        p { class: "no-daemon-lead",
            "{name} runs {runs}, newer than this app's {offer.app}. Update this app to match it."
        }
        div { class: "update-actions",
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
fn RetryOffer(
    machine: MachineId,
    name: String,
    primary: bool,
    busy: Option<DaemonChoice>,
) -> Element {
    let cockpit: Cockpit = use_context();
    let retrying = busy == Some(DaemonChoice::Retry);
    rsx! {
        OfferButton {
            primary,
            disabled: busy.is_some(),
            title: if retrying { "Retrying…".to_owned() } else { "Retry".to_owned() },
            detail: format!("Try {name} again."),
            onclick: move |()| cockpit.retry_machine(machine),
        }
    }
}

#[component]
fn InAppOffer(busy: Option<DaemonChoice>) -> Element {
    let cockpit: Cockpit = use_context();
    let starting = busy == Some(DaemonChoice::InApp);
    rsx! {
        OfferButton {
            primary: false,
            disabled: busy.is_some(),
            title: if starting { "Starting…".to_owned() } else { "Run agents in this app".to_owned() },
            detail: IN_APP_DETAIL.to_owned(),
            onclick: move |()| cockpit.choose_daemon(DaemonChoice::InApp),
        }
    }
}

/// A choice with its title and what it does.
#[component]
fn OfferButton(
    primary: bool,
    disabled: bool,
    title: String,
    detail: String,
    onclick: EventHandler<()>,
) -> Element {
    rsx! {
        button {
            class: if primary { "button primary no-daemon-choice" } else { "button no-daemon-choice" },
            disabled,
            onclick: move |_| onclick.call(()),
            span { class: "no-daemon-choice-title", "{title}" }
            span { class: "no-daemon-choice-detail", "{detail}" }
        }
    }
}

/// The titles of `machine`'s busy agents, in the sidebar's order.
fn busy_titles(state: &AppState, machine: MachineId) -> Vec<String> {
    update_offer::busy_agents(state, machine)
        .into_iter()
        .filter_map(|agent| state.agents.get(&agent))
        .map(|entry| entry.summary.title.clone())
        .collect()
}

/// An update that starts once the machine's agents are idle.
#[component]
fn WaitingCard(machine: MachineId, name: String) -> Element {
    let cockpit: Cockpit = use_context();
    let titles = busy_titles(&cockpit.state.read(), machine);
    rsx! {
        h1 { id: TITLE_ID, "{name} updates once its agents are idle" }
        p { class: "no-daemon-lead",
            "The update starts by itself once none of these agents works or waits for an answer."
        }
        ul { class: "no-daemon-hint",
            for title in titles {
                li { "{title}" }
            }
        }
        div { class: "update-actions",
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

/// Names the machine's busy agents before its update. It takes the close dialog's look until it
/// has a screen of its own.
#[component]
pub fn ConfirmUpdateDialog(machine: MachineId) -> Element {
    let cockpit: Cockpit = use_context();
    let (name, titles) = {
        let state = cockpit.state.read();
        let Some(name) = machine_name(&state, machine) else {
            return rsx! {};
        };
        (name, busy_titles(&state, machine))
    };
    rsx! {
        div {
            class: "dialog close-agent",
            role: "alertdialog",
            aria_label: "Update {name}",
            onclick: move |event| event.stop_propagation(),
            div { class: "dialog-head",
                h2 { "Update {name}" }
            }
            p { class: "dialog-text",
                "A restart ends the turns of these agents. The new daemon resumes them, but what they run now stops."
            }
            ul { class: "dialog-text",
                for title in titles {
                    li { "{title}" }
                }
            }
            div { class: "dialog-actions",
                button {
                    class: "button",
                    onclick: move |_| cockpit.update(|state| state.ui.close_dialog()),
                    "Cancel"
                }
                button {
                    class: "button",
                    onclick: move |_| cockpit.update_daemon_now(machine),
                    "Update now"
                }
                button {
                    class: "button primary",
                    onmounted: move |event| async move {
                        // The dialog may have closed already: nothing to focus then.
                        let _ = event.data().set_focus(true).await;
                    },
                    onclick: move |_| cockpit.update_daemon_when_idle(machine),
                    "Update when they are idle"
                }
            }
        }
    }
}

#[component]
fn ProgressCard(machine: MachineId, name: String, progress: UpdateProgress) -> Element {
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
            "The daemon keeps running until the new version is built. You can close the app: the update continues on the server."
        }
        UpdatePlan::Service(_) => {
            "The app stops the daemon, then starts this app's version. Agents resume after the restart."
        }
    };
    let labels = StepLabels {
        short_commit: match plan {
            UpdatePlan::Server(plan) => plan.commit.short().to_owned(),
            UpdatePlan::Service(_) => String::new(),
        },
        replaces: replaces.as_ref().map(ToString::to_string),
        app: app.to_string(),
    };
    rsx! {
        h1 { id: TITLE_ID, "Updating {name}" }
        p { class: "no-daemon-lead", "{lead}" }
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
            div { class: "update-actions",
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
    short_commit: String,
    /// The daemon's build the update replaces, when the app knew it.
    replaces: Option<String>,
    app: String,
}

impl StepLabels {
    fn of(&self, step: UpdateStep) -> String {
        match step {
            UpdateStep::GetVersion => format!("Get version {}", self.short_commit),
            UpdateStep::Build => "Build it on the server".to_owned(),
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
fn FailedCard(machine: MachineId, name: String, failed: FailedUpdate) -> Element {
    let cockpit: Cockpit = use_context();
    let text = daemon_update::failure_text(&failed, &name);
    let stop_hint = (failed.failure == UpdateFailure::Refused)
        .then(|| failed.first_agent().unwrap_or("<id>").to_owned());
    let again = match failed.plan {
        UpdatePlan::Server(_) => "Update again",
        UpdatePlan::Service(_) => "Retry",
    };
    rsx! {
        h1 { id: TITLE_ID, "{text.title}" }
        p { class: "no-daemon-lead", "{text.lead}" }
        if !failed.message.is_empty() {
            ScrolledOutput { text: failed.message.clone() }
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
        div { class: "update-actions",
            button {
                class: "button primary",
                onclick: move |_| cockpit.update_daemon(machine),
                "{again}"
            }
            if failed.offers_update_now() {
                button {
                    class: "button",
                    onclick: move |_| cockpit.update_daemon_now(machine),
                    "Update now"
                }
            }
            button {
                class: "button",
                onclick: move |_| cockpit.update(|state| daemon_update::back(state, machine)),
                "Back"
            }
        }
    }
}
