//! The "No daemon answers" screen, in place of the window's panes.

use dioxus::prelude::*;

use crate::connection::Cockpit;
use crate::state::ConnectionState;
use crate::state::no_daemon::{
    self, Absence, DaemonChoice, NoDaemon, SERVICE_OFFERED, SOCKET_REACHABLE,
};

/// What the screen says when nothing runs on this machine.
const NOTHING_RUNS_LEAD: &str = if SOCKET_REACHABLE {
    "No daemon runs on this machine yet. Pick one way to run your agents."
} else {
    "On Windows, agents run inside this app. To use a server instead, name it in the config file."
};

const IN_APP_DETAIL: &str =
    "The agents stop when the app closes, and come back the next time the app runs them.";
const SERVICE_DETAIL: &str =
    "Runs agents-cockpit serve on this machine. The agents keep running after the app closes.";

#[component]
pub fn NoDaemonScreen() -> Element {
    let cockpit: Cockpit = use_context();
    let screen = match &cockpit.state.read().machines.this.connection {
        ConnectionState::NoDaemon(screen) => screen.clone(),
        ConnectionState::Connecting { .. }
        | ConnectionState::Connected
        | ConnectionState::Disconnected { .. } => return rsx! {},
    };
    let NoDaemon {
        tried,
        subject,
        error,
        absence,
        every_time,
        busy,
    } = screen;
    let waiting = busy.is_some();
    let every_time_label = rsx! {
        label { class: if waiting { "no-daemon-every-time disabled" } else { "no-daemon-every-time" },
            input {
                r#type: "checkbox",
                checked: every_time,
                disabled: waiting,
                onchange: move |_| cockpit.update(no_daemon::toggle_every_time),
            }
            span { "Use this every time" }
        }
    };
    let body = match absence {
        Absence::NothingRuns => rsx! {
            h1 { id: "no-daemon-title", "Where should agents run?" }
            p { class: "no-daemon-lead", "{NOTHING_RUNS_LEAD}" }
            div { class: "no-daemon-choices",
                Choice {
                    choice: DaemonChoice::InApp,
                    primary: DaemonChoice::InApp,
                    busy,
                    title: "Run agents in this app",
                    busy_title: "Starting…",
                    detail: "{IN_APP_DETAIL}",
                }
                if SERVICE_OFFERED {
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
            div { class: "no-daemon-footer",
                Details { error, open: false }
                if SOCKET_REACHABLE {
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
                if SERVICE_OFFERED {
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
        div { class: "no-daemon",
            div { class: "no-daemon-card", role: "dialog", aria_labelledby: "no-daemon-title",
                {body}
            }
        }
    }
}

/// The error as the daemon's code wrote it, folded unless something failed.
#[component]
fn Details(error: String, open: bool) -> Element {
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
