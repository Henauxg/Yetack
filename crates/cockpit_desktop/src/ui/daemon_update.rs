//! The screens that update a server's daemon of an older version: in the "No daemon answers"
//! screen's place for this computer, as a dialog for a server.

use std::cmp::Ordering;

use dioxus::prelude::*;

use crate::connection::Cockpit;
use crate::machine::MachineId;
use crate::state::ConnectionState;
use crate::state::daemon_update::{
    self, DaemonUpdate, FailedUpdate, ProtocolGap, UpdateFailure, UpdateProgress, UpdateStep,
    VersionGap,
};
use crate::state::no_daemon::DaemonChoice;
use crate::ui::icons::{Icon, IconKind};
use crate::ui::no_daemon::{Details, EveryTimeTick, IN_APP_DETAIL};

const TITLE_ID: &str = "no-daemon-title";

/// The screen `machine`'s state calls for: a card that offers the update, the update under way, or
/// how it failed.
#[component]
pub fn DaemonUpdateScreens(machine: MachineId) -> Element {
    let cockpit: Cockpit = use_context();
    let state = cockpit.state.read();
    let Some(known) = state.machines.get(machine) else {
        return rsx! {};
    };
    let name = known.daemon.machine.clone();
    let subject = known.daemon.subject();
    let body = match (known.update.clone(), known.version_gap.clone()) {
        (Some(DaemonUpdate::Running(progress) | DaemonUpdate::Reconnecting(progress)), _) => {
            rsx! {
                ProgressCard { machine, name, progress }
            }
        }
        (Some(DaemonUpdate::Failed(failed)), _) => rsx! {
            FailedCard { machine, name, failed }
        },
        (None, Some(gap)) => match gap.kind {
            ProtocolGap::Older => rsx! {
                OlderCard { machine, name, subject, gap }
            },
            ProtocolGap::Newer => rsx! {
                NewerCard { machine, name, subject, gap }
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

#[component]
fn OlderCard(machine: MachineId, name: String, subject: String, gap: VersionGap) -> Element {
    let cockpit: Cockpit = use_context();
    let (busy, every_time) = screen_choice(cockpit, machine);
    let waiting = busy.is_some();
    let on_this_computer = machine == MachineId::THIS;
    rsx! {
        h1 { id: TITLE_ID, "{subject} runs an older version" }
        p { class: "no-daemon-lead",
            "This app and the daemon on {name} are different versions, so they cannot connect."
        }
        if gap.plan.is_none() {
            p { class: "no-daemon-hint",
                "This app does not know which version it is, so it cannot update {name}."
            }
        }
        Details { error: gap.details.clone(), open: false }
        div { class: "no-daemon-choices",
            if let Some(plan) = &gap.plan {
                OfferButton {
                    primary: true,
                    disabled: waiting,
                    title: format!("Update {name}"),
                    detail: format!(
                        "Builds this app's version ({}) on the server and restarts its daemon. Agents resume after the restart.",
                        plan.short_commit()
                    ),
                    onclick: move |()| cockpit.update_daemon(machine),
                }
            }
            RetryOffer { machine, name: name.clone(), primary: gap.plan.is_none(), busy }
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
fn NewerCard(machine: MachineId, name: String, subject: String, gap: VersionGap) -> Element {
    let cockpit: Cockpit = use_context();
    let (busy, _) = screen_choice(cockpit, machine);
    rsx! {
        h1 { id: TITLE_ID, "{subject} runs a newer version" }
        p { class: "no-daemon-lead",
            "This app is older than the daemon on {name}, so they cannot connect. Update this app to connect."
        }
        Details { error: gap.details.clone(), open: false }
        div { class: "no-daemon-choices",
            RetryOffer { machine, name: name.clone(), primary: true, busy }
            if machine == MachineId::THIS {
                InAppOffer { busy }
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

#[component]
fn ProgressCard(machine: MachineId, name: String, progress: UpdateProgress) -> Element {
    let cockpit: Cockpit = use_context();
    let UpdateProgress {
        plan,
        step,
        output,
        cancel_asked,
        ..
    } = &progress;
    let output = output
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join("\n");
    rsx! {
        h1 { id: TITLE_ID, "Updating {name}" }
        p { class: "no-daemon-lead",
            "The daemon keeps running until the new version is built. You can close the app: the update continues on the server."
        }
        ol { class: "update-steps",
            for shown in UpdateStep::ALL {
                StepRow { key: "{shown:?}", shown, current: *step, short_commit: plan.short_commit().to_owned() }
            }
        }
        details { class: "no-daemon-details", open: true,
            summary { "Build output" }
            ScrolledOutput { text: output }
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
fn StepRow(shown: UpdateStep, current: UpdateStep, short_commit: String) -> Element {
    let (class, mark) = match shown.cmp(&current) {
        Ordering::Less => ("done", rsx! { Icon { kind: IconKind::Check, size: 14 } }),
        Ordering::Equal => (
            "now",
            rsx! { span { class: "spinner", role: "status", aria_label: "In progress" } },
        ),
        Ordering::Greater => ("", rsx! { Icon { kind: IconKind::Circle, size: 12 } }),
    };
    let label = match shown {
        UpdateStep::GetVersion => format!("Get version {short_commit}"),
        UpdateStep::Build => "Build it on the server".to_owned(),
        UpdateStep::Restart => "Restart the daemon".to_owned(),
        UpdateStep::Reconnect => "Reconnect".to_owned(),
    };
    rsx! {
        li { class,
            span { class: "update-mark", {mark} }
            "{label}"
        }
    }
}

/// The title and the lead of a failure's screen.
fn failure_text(failure: UpdateFailure, name: &str) -> (String, String) {
    let not_updated = format!("{name} was not updated");
    match failure {
        UpdateFailure::Refused => (
            not_updated,
            "These agents are busy, and a restart would interrupt them. Let them finish, then update again.".to_owned(),
        ),
        UpdateFailure::Failed => (
            not_updated,
            format!("The update failed, so {name} still runs its old version."),
        ),
        UpdateFailure::Cancelled => (
            not_updated,
            format!("The update was cancelled, so {name} still runs its old version."),
        ),
        UpdateFailure::InstallFailed => (
            not_updated,
            format!("The update failed during the install or the restart, so the daemon on {name} may not have come back. Check the server."),
        ),
        UpdateFailure::RolledBack => (
            not_updated,
            format!("The new version did not start, so {name} runs its old version again."),
        ),
        UpdateFailure::AlreadyRunning => (
            format!("An update is already running on {name}"),
            "Try again in a few minutes.".to_owned(),
        ),
        UpdateFailure::Unknown => (
            "The app lost track of the update".to_owned(),
            format!("The app cannot tell how the update of {name} went. If it started, it goes on on the server: try again in a few minutes."),
        ),
    }
}

#[component]
fn FailedCard(machine: MachineId, name: String, failed: FailedUpdate) -> Element {
    let cockpit: Cockpit = use_context();
    let (title, lead) = failure_text(failed.failure, &name);
    let stop_hint = (failed.failure == UpdateFailure::Refused)
        .then(|| failed.first_agent().unwrap_or("<id>").to_owned());
    rsx! {
        h1 { id: TITLE_ID, "{title}" }
        p { class: "no-daemon-lead", "{lead}" }
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
        div { class: "update-actions",
            button {
                class: "button primary",
                onclick: move |_| cockpit.update_daemon(machine),
                "Update again"
            }
            button {
                class: "button",
                onclick: move |_| cockpit.update(|state| daemon_update::back(state, machine)),
                "Back"
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cancel_the_user_asked_for_reads_as_one() {
        let lead = |failure| failure_text(failure, "build-box").1;

        assert_eq!(
            lead(UpdateFailure::Cancelled),
            "The update was cancelled, so build-box still runs its old version."
        );
        assert_eq!(
            lead(UpdateFailure::Failed),
            "The update failed, so build-box still runs its old version."
        );
    }

    #[test]
    fn a_rollback_and_another_update_have_their_own_words() {
        let text = |failure| failure_text(failure, "build-box");

        assert_eq!(
            text(UpdateFailure::RolledBack),
            (
                "build-box was not updated".to_owned(),
                "The new version did not start, so build-box runs its old version again."
                    .to_owned()
            )
        );
        assert_eq!(
            text(UpdateFailure::AlreadyRunning),
            (
                "An update is already running on build-box".to_owned(),
                "Try again in a few minutes.".to_owned()
            )
        );
    }
}
