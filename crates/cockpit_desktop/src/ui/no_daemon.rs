//! The "No daemon answers" screen, in place of the window's panes.

use dioxus::prelude::*;

use crate::connection::Cockpit;
use crate::state::ConnectionState;
use crate::state::no_daemon::{DaemonChoice, NoDaemon, SERVICE_OFFERED, toggle_every_time};

#[component]
pub fn NoDaemonScreen() -> Element {
    let cockpit: Cockpit = use_context();
    let screen = match &cockpit.state.read().connection {
        ConnectionState::NoDaemon(screen) => screen.clone(),
        ConnectionState::Connecting { .. }
        | ConnectionState::Connected
        | ConnectionState::Disconnected { .. } => return rsx! {},
    };
    let NoDaemon {
        tried,
        error,
        every_time,
        busy,
    } = screen;
    let waiting = busy.is_some();
    rsx! {
        div { class: "no-daemon",
            div { class: "no-daemon-card", role: "dialog", aria_labelledby: "no-daemon-title",
                h1 { id: "no-daemon-title", "No daemon answers" }
                p { class: "no-daemon-tried", "The app tried {tried}." }
                p { class: "no-daemon-error", "{error}" }
                div { class: "no-daemon-choices",
                    Choice {
                        choice: DaemonChoice::Retry,
                        busy,
                        title: "Retry",
                        busy_title: "Retrying…",
                        detail: "Try {tried} again.",
                    }
                    Choice {
                        choice: DaemonChoice::InApp,
                        busy,
                        title: "Run agents in this app",
                        busy_title: "Starting…",
                        detail: "The agents stop when the app closes, and come back the next time the app runs them.",
                    }
                    if SERVICE_OFFERED {
                        Choice {
                            choice: DaemonChoice::Service,
                            busy,
                            title: "Start the background service here",
                            busy_title: "Starting the service…",
                            detail: "Runs agents-cockpit serve on this machine. The agents keep running after the app closes.",
                        }
                    }
                }
                label { class: if waiting { "no-daemon-every-time disabled" } else { "no-daemon-every-time" },
                    input {
                        r#type: "checkbox",
                        checked: every_time,
                        disabled: waiting,
                        onchange: move |_| cockpit.update(toggle_every_time),
                    }
                    span { "Use this every time" }
                }
                p { class: "no-daemon-hint",
                    if every_time {
                        "The app saves this choice in its config, and starts with it from now on."
                    } else {
                        "Only for now: the next start tries the config's daemon again."
                    }
                }
            }
        }
    }
}

#[component]
fn Choice(
    choice: DaemonChoice,
    busy: Option<DaemonChoice>,
    title: &'static str,
    busy_title: &'static str,
    detail: String,
) -> Element {
    let cockpit: Cockpit = use_context();
    let this_one = busy == Some(choice);
    let class = if choice == DaemonChoice::InApp {
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
