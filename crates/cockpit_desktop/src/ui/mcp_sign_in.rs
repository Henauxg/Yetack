//! The sign-in box on an MCP server's page: "Sign in" while the server needs it, then how the
//! sign-in goes (see `state/mcp_sign_in.rs`).

use std::time::Instant;

use cockpit_protocol::{AgentId, McpServerStatus};
use dioxus::dioxus_core::spawn_forever;
use dioxus::prelude::*;

use crate::connection::{Clock, Cockpit};
use crate::state::mcp_sign_in::{self, Finish, SignIn};
use crate::ui::presence::use_presence;
use crate::{browser, sign_in_tasks};

/// What the box shows.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Shown {
    /// The server needs a sign-in, and none runs.
    Offer,
    Step(SignIn),
}

impl Shown {
    fn class(&self) -> &'static str {
        match self {
            Self::Step(SignIn::SignedIn { .. }) => "mcp-sign-in-box done-box",
            Self::Step(SignIn::Failed(_)) => "mcp-sign-in-box error-box",
            Self::Offer | Self::Step(_) => "mcp-sign-in-box warning-box",
        }
    }
}

#[component]
pub fn SignInBox(
    agent: AgentId,
    server: String,
    status: McpServerStatus,
    machine: String,
) -> Element {
    let cockpit: Cockpit = use_context();
    let clock: Clock = use_context();
    let sign_in = mcp_sign_in::shown(&cockpit.state.read(), agent, &server, Instant::now());
    // "Signed in" leaves after a few seconds: the clock's tick renders the box again.
    if matches!(sign_in, Some(SignIn::SignedIn { .. })) {
        let _ = (clock.0)();
    }
    let current = match sign_in {
        Some(sign_in) => Some(Shown::Step(sign_in)),
        None if status == McpServerStatus::NeedsAuth => Some(Shown::Offer),
        None => None,
    };
    // The box folds shut as it leaves, once the server connected.
    let Some(shown) = use_presence(current) else {
        return rsx! {};
    };
    let class = shown.value.class();
    rsx! {
        div {
            key: "{shown.opening}",
            class: if shown.leaving { "mcp-sign-in leaving" } else { "mcp-sign-in" },
            role: "status",
            inert: shown.leaving.then_some(true),
            div { class: "mcp-sign-in-body",
                div { class: "{class}",
                    SignInBody { agent, server, shown: shown.value, machine }
                }
            }
        }
    }
}

#[component]
fn SignInBody(agent: AgentId, server: String, shown: Shown, machine: String) -> Element {
    let cockpit: Cockpit = use_context();
    let start_server = server.clone();
    let start = move |_| {
        let server = start_server.clone();
        cockpit.change(|state| mcp_sign_in::start(state, agent, &server));
        sign_in_tasks::drive(cockpit);
    };
    match shown {
        Shown::Offer => rsx! {
            p { class: "mcp-sign-in-text", "{server} needs you to sign in." }
            div { class: "card-actions",
                button { class: "button primary small", onclick: start, "Sign in" }
            }
            p { class: "mcp-sign-in-note",
                "The page opens in your browser. The sign-in is saved for this agent's account on {machine}. New agents of the account use it, and running ones once they reconnect."
            }
        },
        Shown::Step(SignIn::Starting) => rsx! {
            Busy { text: "Opening the sign-in page…" }
        },
        Shown::Step(SignIn::InBrowser(waiting)) => rsx! {
            Waiting { agent, server, url: waiting.url, finish: waiting.finish }
        },
        Shown::Step(SignIn::Finishing) => rsx! {
            Busy { text: "Finishing the sign-in…" }
        },
        Shown::Step(SignIn::Connecting { .. }) => rsx! {
            Busy { text: "Signed in. Waiting for the server to connect…" }
        },
        Shown::Step(SignIn::SignedIn { .. }) => rsx! {
            p { class: "mcp-sign-in-text", "Signed in." }
        },
        Shown::Step(SignIn::Failed(reason)) => rsx! {
            p { class: "mcp-sign-in-text", "Sign-in failed: {reason}" }
            div { class: "card-actions",
                button { class: "button small", onclick: start, "Try again" }
            }
            p { class: "mcp-sign-in-note",
                "You can also sign in with /mcp in a terminal on {machine}, with this agent's account."
            }
        },
    }
}

#[component]
fn Busy(text: &'static str) -> Element {
    rsx! {
        p { class: "mcp-sign-in-text busy", aria_busy: true,
            span { class: "spinner", aria_hidden: true }
            "{text}"
        }
    }
}

#[component]
fn Waiting(agent: AgentId, server: String, url: String, finish: Finish) -> Element {
    let cockpit: Cockpit = use_context();
    let mut pasted = use_signal(String::new);
    let mut paste_error = use_signal(|| None::<&'static str>);
    let note = match &finish {
        Finish::Connector => Some("The server connects once the sign-in ends there.".to_owned()),
        Finish::ClaudeListens | Finish::AppListens { .. } => None,
        Finish::Paste { reason } => Some(format!(
            "{reason} After you approve, the browser opens a page that does not load. Copy that page's address and paste it here."
        )),
    };
    let paste_label = match &finish {
        Finish::Paste { .. } => Some("Address of the page"),
        Finish::ClaudeListens | Finish::AppListens { .. } => {
            Some("Page did not load? Paste its address")
        }
        Finish::Connector => None,
    };
    let paste_server = server.clone();
    let mut finish_with_paste = move || match mcp_sign_in::check_pasted(&pasted.read()) {
        Ok(callback_url) => {
            paste_error.set(None);
            let server = paste_server.clone();
            cockpit.change(|state| mcp_sign_in::pasted(state, agent, &server, callback_url));
        }
        Err(error) => paste_error.set(Some(error)),
    };
    let open_again = move |_| {
        let url = url.clone();
        spawn_forever(async move {
            if let Err(error) = browser::open(&url).await {
                tracing::warn!("cannot open the sign-in page: {error}");
                cockpit.update(|state| {
                    state
                        .ui
                        .report(format!("Cannot open the sign-in page: {error}"));
                });
            }
        });
    };
    let cancel_server = server.clone();
    let mut finish_on_enter = finish_with_paste.clone();
    rsx! {
        p { class: "mcp-sign-in-text busy", aria_busy: true,
            span { class: "spinner", aria_hidden: true }
            "Sign in to {server} in your browser."
        }
        if let Some(note) = note {
            p { class: "mcp-sign-in-note", "{note}" }
        }
        if let Some(label) = paste_label {
            span { class: "mcp-sign-in-note", "{label}:" }
            div { class: "mcp-sign-in-paste",
                input {
                    class: "text-field",
                    r#type: "url",
                    placeholder: "http://localhost:…/callback?code=…",
                    aria_label: "{label}",
                    aria_invalid: paste_error().is_some(),
                    value: "{pasted}",
                    oninput: move |event| {
                        pasted.set(event.value());
                        paste_error.set(None);
                    },
                    onkeydown: move |event: KeyboardEvent| {
                        if event.key() == Key::Enter {
                            finish_on_enter();
                        }
                    },
                }
                button {
                    class: "button small",
                    onclick: move |_| finish_with_paste(),
                    "Finish"
                }
            }
            if let Some(error) = paste_error() {
                span { class: "mcp-sign-in-error", role: "alert", "{error}" }
            }
        }
        div { class: "card-actions",
            button { class: "link-button", onclick: open_again, "Open the page again" }
            button {
                class: "link-button",
                onclick: move |_| cockpit.update(|state| mcp_sign_in::cancel(state, agent, &cancel_server)),
                "Cancel"
            }
        }
    }
}
