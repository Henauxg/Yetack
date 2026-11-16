//! The bar above the composer while the selected agent has exited: when its process ended, and
//! Resume. A refused Resume shows the daemon's reason with "Try again", and "Move to another
//! folder…" when the agent's folder is gone.
//!
//! Each agent shows its own: a switch shows the other agent's at once, and only the agent that
//! resumes sees the bar leave.

use cockpit_protocol::{AgentId, AgentStatus};
use dioxus::prelude::*;

use crate::connection::{Clock, Cockpit};
use crate::format;
use crate::state::resume::{self, FolderState, Resume};
use crate::state::{FolderAction, working_folder};
use crate::ui::open_file;
use crate::ui::presence::use_agent_presence;

/// The bar's main button.
const PRIMARY: &str = "button small primary";
/// A button beside the main one.
const SECONDARY: &str = "button small";

#[component]
pub(super) fn ExitedBar() -> Element {
    let cockpit: Cockpit = use_context();
    let state = cockpit.state.read();
    let selected = state.ui.selected;
    let exited = selected.filter(|agent| {
        state
            .agents
            .get(agent)
            .is_some_and(|entry| entry.summary.status == AgentStatus::Exited)
    });
    drop(state);
    let Some(shown) = use_agent_presence(selected, exited) else {
        return rsx! {};
    };
    rsx! {
        div {
            key: "{shown.opening}",
            class: shown.class("exited-bar"),
            inert: shown.leaving.then_some(true),
            role: "status",
            // A leaving bar belongs to an agent that runs again.
            BarContent { agent: shown.agent, resuming: shown.leaving }
        }
    }
}

#[component]
fn BarContent(agent: AgentId, resuming: bool) -> Element {
    let cockpit: Cockpit = use_context();
    let Clock(now) = use_context::<Clock>();
    let state = cockpit.state.read();
    let Some(entry) = state.agents.get(&agent) else {
        return rsx! {};
    };
    let resume_button = move |class: &'static str, label: &'static str| {
        rsx! {
            button {
                class,
                onclick: move |_| cockpit.change(|state| resume::send(state, agent)),
                "{label}"
            }
        }
    };
    if let Some(refusal) = resume::refusal(entry) {
        let reason = open_file::sentence(&refusal.reason);
        let folder_gone = refusal.folder_state == FolderState::Gone
            && working_folder::blocked(&state, agent, FolderAction::Move).is_none();
        return rsx! {
            span { class: "exited-state refused", "Cannot start" }
            span { class: "exited-text", title: "{reason}", "{reason}" }
            {resume_button(if folder_gone { SECONDARY } else { PRIMARY }, "Try again")}
            if folder_gone {
                button {
                    class: PRIMARY,
                    onclick: move |_| cockpit.update_with_drafts(|state, drafts| {
                        working_folder::open(state, drafts, agent, FolderAction::Move);
                    }),
                    "Move to another folder…"
                }
            }
        };
    }
    let ended = match entry.summary.last_activity_at {
        Some(moment) => format!(
            "The claude process ended {}.",
            format::ago_in_sentence(moment, now())
        ),
        None => "The claude process ended.".to_owned(),
    };
    let text = format!("{ended} Resume starts it again on the same conversation.");
    let sending = resuming || entry.resume == Resume::Sending;
    rsx! {
        span { class: "exited-state", "Exited" }
        span { class: "exited-text", title: "{text}", "{text}" }
        if sending {
            button { class: PRIMARY, disabled: true, "Resuming…" }
        } else {
            {resume_button(PRIMARY, "Resume")}
        }
    }
}
