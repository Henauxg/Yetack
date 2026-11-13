//! The "No daemon answers" screen, in place of the window's panes.

use dioxus::prelude::*;

use crate::connection::Cockpit;
use crate::machine::MachineId;
use crate::state::no_daemon::{self, Absence, DaemonChoice, NoDaemon};
use crate::state::{ConnectionState, settings};
use crate::ui::daemon_update::DaemonUpdateScreens;

/// What the screen says when nothing runs on this machine, where the background service runs.
const NOTHING_RUNS_LEAD: &str =
    "No daemon runs on this machine yet. Pick one way to run your agents.";
/// What the screen says when nothing runs on this machine, on Windows.
const NOTHING_RUNS_LEAD_IN_APP: &str =
    "On Windows, agents run inside this app. To use a server instead, add it in Machines.";

pub(super) const IN_APP_DETAIL: &str =
    "The agents stop when the app closes, and come back the next time the app runs them.";
const SERVICE_DETAIL: &str = "Runs the daemon in the background on this machine. The agents keep running after the app closes.";

/// The screen in place of the window's panes, while the app reaches this computer alone.
#[component]
pub fn NoDaemonScreen() -> Element {
    rsx! {
        div { class: "no-daemon",
            NoDaemonCard {}
        }
    }
}

/// The question and its choices, on the screen or in a dialog.
#[component]
pub fn NoDaemonCard() -> Element {
    let cockpit: Cockpit = use_context();
    let update_screens = {
        let state = cockpit.state.read();
        state.machines.this.update.is_some() || state.machines.this.version_gap.is_some()
    };
    if update_screens {
        return rsx! {
            DaemonUpdateScreens { machine: MachineId::THIS }
        };
    }
    let (screen, service_offered) = {
        let state = cockpit.state.read();
        let screen = match &state.machines.this.connection {
            ConnectionState::NoDaemon(screen) => screen.clone(),
            ConnectionState::Connecting { .. }
            | ConnectionState::Connected
            | ConnectionState::Disconnected { .. } => return rsx! {},
        };
        (screen, state.machines.service_offered)
    };
    let nothing_runs_lead = if service_offered {
        NOTHING_RUNS_LEAD
    } else {
        NOTHING_RUNS_LEAD_IN_APP
    };
    let NoDaemon {
        tried,
        subject,
        error,
        absence,
        every_time,
        failed_tries,
        busy,
    } = screen;
    let failure = no_daemon::failure_note(failed_tries);
    let failure_line = rsx! {
        if let Some(failure) = failure {
            p { class: "no-daemon-failed", role: "alert", "{failure}" }
        }
    };
    let waiting = busy.is_some();
    let every_time_label = rsx! {
        EveryTimeTick { every_time, waiting }
    };
    let body = match absence {
        Absence::NothingRuns => rsx! {
            h1 { id: "no-daemon-title", "Where should agents run?" }
            p { class: "no-daemon-lead", "{nothing_runs_lead}" }
            div { class: "no-daemon-choices",
                Choice {
                    choice: DaemonChoice::InApp,
                    primary: DaemonChoice::InApp,
                    busy,
                    title: "Run agents in this app",
                    busy_title: "Starting…",
                    detail: "{IN_APP_DETAIL}",
                }
                if service_offered {
                    Choice {
                        choice: DaemonChoice::Service,
                        primary: DaemonChoice::InApp,
                        busy,
                        title: "Start the background service here",
                        busy_title: "Starting the service…",
                        detail: "{SERVICE_DETAIL}",
                    }
                }
            }
            {every_time_label}
            {failure_line}
            div { class: "no-daemon-footer",
                Details { error, open: failed_tries > 0 }
                if service_offered {
                    button {
                        class: "no-daemon-link",
                        disabled: waiting,
                        onclick: move |_| cockpit.choose_daemon(DaemonChoice::Retry),
                        if busy == Some(DaemonChoice::Retry) { "Looking for a daemon…" } else { "Look for a daemon again" }
                    }
                }
            }
        },
        Absence::Failed => rsx! {
            h1 { id: "no-daemon-title", "{subject} does not answer" }
            p { class: "no-daemon-lead", "The app could not reach {tried}." }
            {failure_line}
            Details { error, open: true }
            div { class: "no-daemon-choices",
                Choice {
                    choice: DaemonChoice::Retry,
                    primary: DaemonChoice::Retry,
                    busy,
                    title: "Retry",
                    busy_title: "Retrying…",
                    detail: "Try {tried} again.",
                }
                Choice {
                    choice: DaemonChoice::InApp,
                    primary: DaemonChoice::Retry,
                    busy,
                    title: "Run agents in this app",
                    busy_title: "Starting…",
                    detail: "{IN_APP_DETAIL}",
                }
                if service_offered {
                    Choice {
                        choice: DaemonChoice::Service,
                        primary: DaemonChoice::Retry,
                        busy,
                        title: "Start the background service here",
                        busy_title: "Starting the service…",
                        detail: "{SERVICE_DETAIL}",
                    }
                }
            }
            {every_time_label}
            p { class: "no-daemon-hint",
                if every_time {
                    "The app saves this choice in its config, and starts with it from now on."
                } else {
                    "Only for now: the next start tries the config's daemon again."
                }
            }
        },
    };
    rsx! {
        div {
            class: "no-daemon-card",
            role: "dialog",
            aria_labelledby: "no-daemon-title",
            onclick: move |event| event.stop_propagation(),
            {body}
        }
    }
}

/// "Use this every time", for a choice about this computer's daemon, and where to change it
/// later where the Settings dialog offers a choice.
#[component]
pub(super) fn EveryTimeTick(every_time: bool, waiting: bool) -> Element {
    let cockpit: Cockpit = use_context();
    let change_later = settings::change_later(&cockpit.state.read());
    rsx! {
        label { class: if waiting { "no-daemon-every-time disabled" } else { "no-daemon-every-time" },
            input {
                r#type: "checkbox",
                checked: every_time,
                disabled: waiting,
                onchange: move |_| cockpit.update(no_daemon::toggle_every_time),
            }
            span { "Use this every time" }
        }
        if let Some(hint) = change_later {
            p { class: "no-daemon-hint", "{hint}" }
        }
    }
}

/// The error as the daemon's code wrote it, folded unless something failed.
#[component]
pub(super) fn Details(error: String, open: bool) -> Element {
    rsx! {
        details { class: "no-daemon-details", open,
            summary { "Details" }
            pre { "{error}" }
        }
    }
}

#[component]
fn Choice(
    choice: DaemonChoice,
    /// The choice the screen offers first.
    primary: DaemonChoice,
    busy: Option<DaemonChoice>,
    title: &'static str,
    busy_title: &'static str,
    detail: String,
) -> Element {
    let cockpit: Cockpit = use_context();
    let this_one = busy == Some(choice);
    let class = if choice == primary {
        "button primary no-daemon-choice"
    } else {
        "button no-daemon-choice"
    };
    rsx! {
        button {
            class,
            disabled: busy.is_some(),
            onclick: move |_| cockpit.choose_daemon(choice),
            span { class: "no-daemon-choice-title", if this_one { "{busy_title}" } else { "{title}" } }
            span { class: "no-daemon-choice-detail", "{detail}" }
        }
    }
}
